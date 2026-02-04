use crate::error::GitAiError;
use std::path::PathBuf;

/// Parameters passed to hook installers
#[derive(Clone)]
pub struct HookInstallerParams {
    /// Path to the git-ai binary
    pub binary_path: PathBuf,
}

/// Result of checking hook status
pub struct HookCheckResult {
    /// Whether the tool (IDE/agent) is installed
    pub tool_installed: bool,
    /// Whether hooks are installed
    pub hooks_installed: bool,
    /// Whether hooks are up to date
    #[allow(dead_code)]
    pub hooks_up_to_date: bool,
}

/// Result of an install operation
pub struct InstallResult {
    /// Whether changes were made
    pub changed: bool,
    /// Diff output if changes were made
    pub diff: Option<String>,
    /// Human-readable message
    pub message: String,
}

/// Result of an uninstall operation
pub struct UninstallResult {
    /// Whether changes were made
    pub changed: bool,
    /// Diff output if changes were made
    pub diff: Option<String>,
    /// Human-readable message
    pub message: String,
}

/// Trait for installing hooks into various IDEs and agent configurations
pub trait HookInstaller: Send + Sync {
    /// Human-readable name of the tool (e.g., "Claude Code", "Cursor")
    fn name(&self) -> &str;

    /// Short identifier for status maps (e.g., "claude-code", "cursor")
    fn id(&self) -> &str;

    /// Whether this tool uses config file hooks (vs only extras like plugins)
    /// Default is true. Tools that only use install_extras should return false.
    fn uses_config_hooks(&self) -> bool {
        true
    }

    /// Check if the tool is installed and hook status
    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError>;

    /// Install or update hooks
    /// Returns Ok(Some(diff)) if changes were made, Ok(None) if already up to date
    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError>;

    /// Uninstall hooks
    /// Returns Ok(Some(diff)) if changes were made, Ok(None) if nothing to uninstall
    fn uninstall_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError>;

    /// Install extras (e.g., VS Code extensions, git.path configuration)
    /// Default implementation does nothing
    fn install_extras(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        Ok(vec![])
    }

    /// Uninstall extras (e.g., VS Code extensions, git.path configuration)
    /// Default implementation does nothing
    fn uninstall_extras(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<UninstallResult>, GitAiError> {
        Ok(vec![])
    }
}
