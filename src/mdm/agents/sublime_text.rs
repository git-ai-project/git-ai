use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult,
};
use crate::mdm::utils::binary_exists;
#[cfg(not(windows))]
use crate::mdm::utils::home_dir;
use std::fs;
use std::path::PathBuf;

// Plugin source, embedded at compile time with binary path placeholder
const PLUGIN_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/sublime-text/git_ai.py"
));

pub struct SublimeTextInstaller;

impl SublimeTextInstaller {
    fn packages_dir() -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            Some(
                home_dir()
                    .join("Library")
                    .join("Application Support")
                    .join("Sublime Text")
                    .join("Packages"),
            )
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // Try both Sublime Text 3 and 4 paths
            let paths = [
                home_dir()
                    .join(".config")
                    .join("sublime-text")
                    .join("Packages"),
                home_dir()
                    .join(".config")
                    .join("sublime-text-3")
                    .join("Packages"),
            ];
            paths.into_iter().find(|p| p.exists()).or_else(|| {
                Some(
                    home_dir()
                        .join(".config")
                        .join("sublime-text")
                        .join("Packages"),
                )
            })
        }
        #[cfg(windows)]
        {
            std::env::var("APPDATA")
                .ok()
                .map(|appdata| PathBuf::from(appdata).join("Sublime Text").join("Packages"))
        }
    }

    fn plugin_path() -> Option<PathBuf> {
        Self::packages_dir().map(|p| p.join("git-ai").join("git_ai.py"))
    }

    fn is_plugin_installed(binary_path: &std::path::Path) -> bool {
        let Some(path) = Self::plugin_path() else {
            return false;
        };
        if !path.exists() {
            return false;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            return false;
        };
        content.contains(&binary_path.display().to_string())
    }
}

impl HookInstaller for SublimeTextInstaller {
    fn name(&self) -> &str {
        "Sublime Text"
    }

    fn id(&self) -> &str {
        "sublime-text"
    }

    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_subl = binary_exists("subl") || binary_exists("sublime_text");
        let has_packages = Self::packages_dir().map(|p| p.exists()).unwrap_or(false);
        let tool_installed = has_subl || has_packages;

        let hooks_installed = Self::is_plugin_installed(&params.binary_path);
        Ok(HookCheckResult {
            tool_installed,
            hooks_installed,
            hooks_up_to_date: hooks_installed,
        })
    }

    fn install_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Ok(None)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Ok(None)
    }

    fn install_extras(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        let Some(plugin_path) = Self::plugin_path() else {
            return Ok(vec![InstallResult {
                changed: false,
                diff: None,
                message: "Sublime Text: Could not determine Packages directory".to_string(),
            }]);
        };

        if Self::is_plugin_installed(&params.binary_path) {
            return Ok(vec![InstallResult {
                changed: false,
                diff: None,
                message: "Sublime Text: Plugin already installed".to_string(),
            }]);
        }

        if dry_run {
            return Ok(vec![InstallResult {
                changed: true,
                diff: None,
                message: format!(
                    "Sublime Text: Pending plugin install to {}",
                    plugin_path.display()
                ),
            }]);
        }

        // Substitute the binary path placeholder
        let path_str = params
            .binary_path
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let content = PLUGIN_TEMPLATE.replace("__GIT_AI_BINARY_PATH__", &path_str);

        if let Some(dir) = plugin_path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&plugin_path, content)?;

        Ok(vec![InstallResult {
            changed: true,
            diff: None,
            message: format!(
                "Sublime Text: Plugin installed to {} (hot-reloaded, no restart needed)",
                plugin_path.display()
            ),
        }])
    }

    fn uninstall_extras(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<UninstallResult>, GitAiError> {
        let Some(plugin_path) = Self::plugin_path() else {
            return Ok(vec![UninstallResult {
                changed: false,
                diff: None,
                message: "Sublime Text: Could not determine Packages directory".to_string(),
            }]);
        };
        if let Some(dir) = plugin_path.parent()
            && dir.exists()
        {
            fs::remove_dir_all(dir)?;
            return Ok(vec![UninstallResult {
                changed: true,
                diff: None,
                message: "Sublime Text: Plugin removed".to_string(),
            }]);
        }
        Ok(vec![UninstallResult {
            changed: false,
            diff: None,
            message: "Sublime Text: Plugin was not installed".to_string(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sublime_text_installer_name() {
        assert_eq!(SublimeTextInstaller.name(), "Sublime Text");
    }

    #[test]
    fn test_sublime_text_installer_id() {
        assert_eq!(SublimeTextInstaller.id(), "sublime-text");
    }

    #[test]
    fn test_sublime_text_install_hooks_returns_none() {
        let installer = SublimeTextInstaller;
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        assert!(installer.install_hooks(&params, false).unwrap().is_none());
    }
}
