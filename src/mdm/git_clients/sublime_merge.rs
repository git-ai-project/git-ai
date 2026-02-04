use crate::error::GitAiError;
use crate::mdm::git_client_installer::{
    GitClientCheckResult, GitClientInstaller, GitClientInstallerParams,
};
use crate::mdm::utils::{home_dir, write_atomic};
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::CstRootNode;
use std::fs;
use std::path::PathBuf;

pub struct SublimeMergeInstaller;

impl SublimeMergeInstaller {
    /// Get the path to Sublime Merge preferences file
    fn prefs_path() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            home_dir()
                .join("Library")
                .join("Application Support")
                .join("Sublime Merge")
                .join("Packages")
                .join("User")
                .join("Preferences.sublime-settings")
        }
        #[cfg(windows)]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                PathBuf::from(appdata)
                    .join("Sublime Merge")
                    .join("Packages")
                    .join("User")
                    .join("Preferences.sublime-settings")
            } else {
                home_dir()
                    .join("AppData")
                    .join("Roaming")
                    .join("Sublime Merge")
                    .join("Packages")
                    .join("User")
                    .join("Preferences.sublime-settings")
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            home_dir()
                .join(".config")
                .join("sublime-merge")
                .join("Packages")
                .join("User")
                .join("Preferences.sublime-settings")
        }
    }

    /// Check if Sublime Merge is installed
    fn is_installed() -> bool {
        #[cfg(target_os = "macos")]
        {
            // Check for app bundle or preferences directory
            let app_path = std::path::Path::new("/Applications/Sublime Merge.app");
            let prefs_dir = home_dir()
                .join("Library")
                .join("Application Support")
                .join("Sublime Merge");
            app_path.exists() || prefs_dir.exists()
        }
        #[cfg(windows)]
        {
            // Check for preferences directory
            let prefs_dir = if let Ok(appdata) = std::env::var("APPDATA") {
                PathBuf::from(appdata).join("Sublime Merge")
            } else {
                home_dir()
                    .join("AppData")
                    .join("Roaming")
                    .join("Sublime Merge")
            };
            prefs_dir.exists()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // Check for preferences directory
            let prefs_dir = home_dir().join(".config").join("sublime-merge");
            prefs_dir.exists()
        }
    }

    /// Read the current git_binary setting from preferences
    fn read_git_binary() -> Option<String> {
        let prefs_path = Self::prefs_path();
        if !prefs_path.exists() {
            return None;
        }

        let content = fs::read_to_string(&prefs_path).ok()?;
        let parse_options = ParseOptions::default();
        let root = CstRootNode::parse(&content, &parse_options).ok()?;
        let obj = root.object_value()?;
        let prop = obj.get("git_binary")?;
        let value = prop.value()?;
        let string_lit = value.as_string_lit()?;
        string_lit.decoded_value().ok()
    }
}

impl GitClientInstaller for SublimeMergeInstaller {
    fn name(&self) -> &str {
        "Sublime Merge"
    }

    fn id(&self) -> &str {
        "sublime-merge"
    }

    fn is_platform_supported(&self) -> bool {
        // Sublime Merge is supported on all platforms
        true
    }

    fn check_client(
        &self,
        params: &GitClientInstallerParams,
    ) -> Result<GitClientCheckResult, GitAiError> {
        if !Self::is_installed() {
            return Ok(GitClientCheckResult {
                client_installed: false,
                prefs_configured: false,
                prefs_up_to_date: false,
            });
        }

        let current_git_binary = Self::read_git_binary();
        // Use forward slashes for JSON compatibility on Windows
        let desired_path = params.git_shim_path.to_string_lossy().replace('\\', "/");

        let prefs_configured = current_git_binary.is_some();
        let prefs_up_to_date = current_git_binary
            .as_ref()
            .map(|p| p == &desired_path)
            .unwrap_or(false);

        Ok(GitClientCheckResult {
            client_installed: true,
            prefs_configured,
            prefs_up_to_date,
        })
    }

    fn install_prefs(
        &self,
        params: &GitClientInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let check = self.check_client(params)?;

        if !check.client_installed {
            return Ok(None);
        }

        if check.prefs_up_to_date {
            return Ok(None);
        }

        let prefs_path = Self::prefs_path();
        // Use forward slashes for JSON compatibility on Windows
        let git_wrapper_path = params.git_shim_path.to_string_lossy().replace('\\', "/");

        // Read existing content
        let original = if prefs_path.exists() {
            fs::read_to_string(&prefs_path)?
        } else {
            String::new()
        };

        // Parse as JSONC (supports comments and trailing commas)
        let parse_input = if original.trim().is_empty() {
            "{}".to_string()
        } else {
            original.clone()
        };

        let parse_options = ParseOptions::default();
        let root = CstRootNode::parse(&parse_input, &parse_options).map_err(|err| {
            GitAiError::Generic(format!("Failed to parse {}: {}", prefs_path.display(), err))
        })?;

        let object = root.object_value_or_set();

        // Check if we need to update
        let mut changed = false;

        match object.get("git_binary") {
            Some(prop) => {
                let should_update = match prop.value() {
                    Some(node) => match node.as_string_lit() {
                        Some(string_node) => match string_node.decoded_value() {
                            Ok(existing_value) => existing_value != git_wrapper_path,
                            Err(_) => true,
                        },
                        None => true,
                    },
                    None => true,
                };

                if should_update {
                    prop.set_value(jsonc_parser::json!(git_wrapper_path.as_str()));
                    changed = true;
                }
            }
            None => {
                object.append("git_binary", jsonc_parser::json!(git_wrapper_path.as_str()));
                changed = true;
            }
        }

        if !changed {
            return Ok(None);
        }

        let new_content = root.to_string();
        let diff_output = format!(
            "+++ {}\n+git_binary = {}\n",
            prefs_path.display(),
            git_wrapper_path
        );

        if !dry_run {
            // Ensure parent directory exists
            if let Some(parent) = prefs_path.parent()
                && !parent.exists() {
                    fs::create_dir_all(parent)?;
                }
            write_atomic(&prefs_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_prefs(
        &self,
        params: &GitClientInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let check = self.check_client(params)?;

        if !check.client_installed || !check.prefs_configured {
            return Ok(None);
        }

        let prefs_path = Self::prefs_path();
        if !prefs_path.exists() {
            return Ok(None);
        }

        let original = fs::read_to_string(&prefs_path)?;

        let parse_options = ParseOptions::default();
        let root = CstRootNode::parse(&original, &parse_options).map_err(|err| {
            GitAiError::Generic(format!("Failed to parse {}: {}", prefs_path.display(), err))
        })?;

        let object = match root.object_value() {
            Some(obj) => obj,
            None => return Ok(None),
        };

        // Remove the git_binary property
        let prop = match object.get("git_binary") {
            Some(p) => p,
            None => return Ok(None),
        };

        // Get the old value for diff output
        let old_git_binary = prop
            .value()
            .and_then(|v| v.as_string_lit())
            .and_then(|s| s.decoded_value().ok())
            .unwrap_or_default();

        prop.remove();

        let new_content = root.to_string();
        let diff_output = format!(
            "--- {}\n-git_binary = {}\n",
            prefs_path.display(),
            old_git_binary
        );

        if !dry_run {
            write_atomic(&prefs_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}
