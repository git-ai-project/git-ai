use crate::error::GitAiError;
use crate::mdm::git_client_installer::{
    GitClientCheckResult, GitClientInstaller, GitClientInstallerParams,
};

#[cfg(target_os = "macos")]
use super::mac_prefs::{Preferences, find_app_by_bundle_id};

#[cfg(windows)]
use crate::mdm::utils::{home_dir, write_atomic};
#[cfg(windows)]
use serde_json::{Value, json};
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::path::PathBuf;

/// Fork.app bundle identifier (macOS)
#[cfg(target_os = "macos")]
const FORK_BUNDLE_ID: &str = "com.DanPristupov.Fork";

/// Git instance type values for Fork
#[allow(dead_code)]
mod git_instance_type {
    pub const SYSTEM: i32 = 0;
    pub const BUNDLED: i32 = 1;
    pub const CUSTOM: i32 = 2;
}

pub struct ForkAppInstaller;

// ============================================================================
// macOS Implementation
// ============================================================================

#[cfg(target_os = "macos")]
impl ForkAppInstaller {
    /// Get Fork.app preferences handle
    fn prefs() -> Preferences {
        Preferences::new(FORK_BUNDLE_ID)
    }

    /// Check if Fork.app is installed
    fn is_fork_installed() -> bool {
        // Check if Fork.app exists via bundle ID
        if find_app_by_bundle_id(FORK_BUNDLE_ID).is_some() {
            return true;
        }

        false
    }
}

impl GitClientInstaller for ForkAppInstaller {
    fn name(&self) -> &str {
        "Fork"
    }

    fn id(&self) -> &str {
        "fork"
    }

    fn is_platform_supported(&self) -> bool {
        cfg!(target_os = "macos") || cfg!(windows)
    }

    // ========================================================================
    // macOS trait implementations
    // ========================================================================

    #[cfg(target_os = "macos")]
    fn check_client(
        &self,
        params: &GitClientInstallerParams,
    ) -> Result<GitClientCheckResult, GitAiError> {
        if !Self::is_fork_installed() {
            return Ok(GitClientCheckResult {
                client_installed: false,
                prefs_configured: false,
                prefs_up_to_date: false,
            });
        }

        let prefs = Self::prefs();
        let git_type = prefs.read_int("gitInstanceType");
        let custom_path = prefs.read_string("customGitInstancePath");

        let is_custom = git_type == Some(git_instance_type::CUSTOM);
        let path_matches = custom_path
            .as_ref()
            .map(|p| p == params.git_shim_path.to_string_lossy().as_ref())
            .unwrap_or(false);

        Ok(GitClientCheckResult {
            client_installed: true,
            prefs_configured: is_custom && custom_path.is_some(),
            prefs_up_to_date: is_custom && path_matches,
        })
    }

    #[cfg(target_os = "macos")]
    fn install_prefs(
        &self,
        params: &GitClientInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let check = self.check_client(params)?;

        if !check.client_installed || check.prefs_up_to_date {
            return Ok(None);
        }

        let prefs = Self::prefs();
        let git_wrapper_path = params.git_shim_path.to_string_lossy();

        let diff = format!(
            "+++ {}\n+gitInstanceType = {}\n+customGitInstancePath = {}\n",
            FORK_BUNDLE_ID,
            git_instance_type::CUSTOM,
            git_wrapper_path
        );

        if !dry_run {
            prefs.write_int("gitInstanceType", git_instance_type::CUSTOM)?;
            prefs.write_string("customGitInstancePath", &git_wrapper_path)?;
        }

        Ok(Some(diff))
    }

    #[cfg(target_os = "macos")]
    fn uninstall_prefs(
        &self,
        params: &GitClientInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let check = self.check_client(params)?;

        if !check.client_installed || !check.prefs_configured {
            return Ok(None);
        }

        let prefs = Self::prefs();
        let old_type = prefs.read_int("gitInstanceType").unwrap_or(0);
        let old_path = prefs
            .read_string("customGitInstancePath")
            .unwrap_or_default();

        let mut diff = format!("--- {}\n-gitInstanceType = {}\n", FORK_BUNDLE_ID, old_type);
        if !old_path.is_empty() {
            diff.push_str(&format!("-customGitInstancePath = {}\n", old_path));
        }

        if !dry_run {
            prefs.write_int("gitInstanceType", git_instance_type::SYSTEM)?;
            let _ = prefs.delete("customGitInstancePath");
        }

        Ok(Some(diff))
    }

    // ========================================================================
    // Windows trait implementations
    // ========================================================================

    #[cfg(windows)]
    fn check_client(
        &self,
        params: &GitClientInstallerParams,
    ) -> Result<GitClientCheckResult, GitAiError> {
        if !Self::is_fork_installed() {
            return Ok(GitClientCheckResult {
                client_installed: false,
                prefs_configured: false,
                prefs_up_to_date: false,
            });
        }

        let git_type = Self::read_git_instance_type();
        let custom_path = Self::read_custom_git_path();

        let is_custom = git_type == Some(git_instance_type::CUSTOM);
        let path_matches = custom_path
            .as_ref()
            .map(|p| p == params.git_shim_path.to_string_lossy().as_ref())
            .unwrap_or(false);

        let prefs_configured = is_custom && custom_path.is_some();
        let prefs_up_to_date = is_custom && path_matches;

        Ok(GitClientCheckResult {
            client_installed: true,
            prefs_configured,
            prefs_up_to_date,
        })
    }

    #[cfg(windows)]
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

        let settings_path = Self::settings_path();
        let git_wrapper_path = params.git_shim_path.to_string_lossy().into_owned();

        // Read existing settings
        let original = if settings_path.exists() {
            fs::read_to_string(&settings_path)?
        } else {
            String::new()
        };

        let mut settings: Value = if original.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&original)?
        };

        let diff = format!(
            "+++ {}\n+GitInstanceType = {}\n+CustomGitInstancePath = {}\n",
            settings_path.display(),
            git_instance_type::CUSTOM,
            git_wrapper_path
        );

        if !dry_run {
            if let Some(obj) = settings.as_object_mut() {
                obj.insert(
                    "GitInstanceType".to_string(),
                    json!(git_instance_type::CUSTOM),
                );
                obj.insert("CustomGitInstancePath".to_string(), json!(git_wrapper_path));
            }

            // Ensure parent directory exists
            if let Some(parent) = settings_path.parent()
                && !parent.exists()
            {
                fs::create_dir_all(parent)?;
            }

            let new_content = serde_json::to_string_pretty(&settings)?;
            write_atomic(&settings_path, new_content.as_bytes())?;
        }

        Ok(Some(diff))
    }

    #[cfg(windows)]
    fn uninstall_prefs(
        &self,
        params: &GitClientInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let check = self.check_client(params)?;

        if !check.client_installed || !check.prefs_configured {
            return Ok(None);
        }

        let settings_path = Self::settings_path();
        if !settings_path.exists() {
            return Ok(None);
        }

        let original = fs::read_to_string(&settings_path)?;
        let mut settings: Value = serde_json::from_str(&original)?;

        let old_type = settings
            .get("GitInstanceType")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
            .unwrap_or(0);
        let old_path = settings
            .get("CustomGitInstancePath")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut diff = format!(
            "--- {}\n-GitInstanceType = {}\n",
            settings_path.display(),
            old_type
        );
        if !old_path.is_empty() {
            diff.push_str(&format!("-CustomGitInstancePath = {}\n", old_path));
        }

        if !dry_run {
            if let Some(obj) = settings.as_object_mut() {
                obj.insert(
                    "GitInstanceType".to_string(),
                    json!(git_instance_type::SYSTEM),
                );
                obj.remove("CustomGitInstancePath");
            }

            let new_content = serde_json::to_string_pretty(&settings)?;
            write_atomic(&settings_path, new_content.as_bytes())?;
        }

        Ok(Some(diff))
    }

    // ========================================================================
    // Unsupported platforms (Linux)
    // ========================================================================

    #[cfg(all(unix, not(target_os = "macos")))]
    fn check_client(
        &self,
        _params: &GitClientInstallerParams,
    ) -> Result<GitClientCheckResult, GitAiError> {
        Ok(GitClientCheckResult {
            client_installed: false,
            prefs_configured: false,
            prefs_up_to_date: false,
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn install_prefs(
        &self,
        _params: &GitClientInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Ok(None)
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn uninstall_prefs(
        &self,
        _params: &GitClientInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Ok(None)
    }
}

// ============================================================================
// Windows Implementation
// ============================================================================

#[cfg(windows)]
impl ForkAppInstaller {
    /// Get the path to Fork settings file on Windows
    fn settings_path() -> PathBuf {
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(localappdata)
                .join("Fork")
                .join("settings.json")
        } else {
            home_dir()
                .join("AppData")
                .join("Local")
                .join("Fork")
                .join("settings.json")
        }
    }

    /// Check if Fork is installed on Windows
    fn is_fork_installed() -> bool {
        // Check for settings directory
        let settings_dir = if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(localappdata).join("Fork")
        } else {
            home_dir().join("AppData").join("Local").join("Fork")
        };

        // Also check Program Files
        let program_files = std::env::var("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:\\Program Files"));
        let app_path = program_files.join("Fork").join("Fork.exe");

        settings_dir.exists() || app_path.exists()
    }

    /// Read settings from JSON file
    fn read_settings() -> Option<Value> {
        let settings_path = Self::settings_path();
        if !settings_path.exists() {
            return None;
        }
        let content = fs::read_to_string(&settings_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Read GitInstanceType from settings
    fn read_git_instance_type() -> Option<i32> {
        let settings = Self::read_settings()?;
        settings.get("GitInstanceType")?.as_i64().map(|v| v as i32)
    }

    /// Read CustomGitInstancePath from settings
    fn read_custom_git_path() -> Option<String> {
        let settings = Self::read_settings()?;
        settings
            .get("CustomGitInstancePath")?
            .as_str()
            .map(|s| s.to_string())
    }
}
