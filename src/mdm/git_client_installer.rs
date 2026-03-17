use crate::error::GitAiError;
use std::path::PathBuf;

/// Parameters passed to git client installers
#[derive(Clone)]
pub struct GitClientInstallerParams {
    /// Path to the git shim that clients should use (e.g., ~/.local/bin/git)
    /// This is the symlink/shim that points to git-ai, not git-ai itself
    pub git_shim_path: PathBuf,
}

/// Result of checking git client configuration status
pub struct GitClientCheckResult {
    /// Whether the git client application is installed
    pub client_installed: bool,
    /// Whether git-ai preferences are configured
    pub prefs_configured: bool,
    /// Whether the preferences are up to date
    pub prefs_up_to_date: bool,
}

/// Trait for configuring git client applications to use git-ai
pub trait GitClientInstaller: Send + Sync {
    /// Human-readable name of the client (e.g., "Fork.app", "GitHub Desktop")
    fn name(&self) -> &str;

    /// Short identifier for status maps (e.g., "fork-app", "github-desktop")
    fn id(&self) -> &str;

    /// Check if this installer is supported on the current platform
    fn is_platform_supported(&self) -> bool;

    /// Check if the client is installed and preference status
    fn check_client(
        &self,
        params: &GitClientInstallerParams,
    ) -> Result<GitClientCheckResult, GitAiError>;

    /// Install or update preferences to use git-ai
    /// Returns Ok(Some(diff)) if changes were made, Ok(None) if already up to date
    fn install_prefs(
        &self,
        params: &GitClientInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError>;

    /// Uninstall preferences (revert to system git)
    /// Returns Ok(Some(diff)) if changes were made, Ok(None) if nothing to uninstall
    fn uninstall_prefs(
        &self,
        params: &GitClientInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError>;
}
