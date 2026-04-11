use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult,
};
use std::path::Path;

pub struct XcodeInstaller;

impl HookInstaller for XcodeInstaller {
    fn name(&self) -> &str {
        "Xcode"
    }

    fn id(&self) -> &str {
        "xcode"
    }

    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed = is_xcode_installed();
        Ok(HookCheckResult {
            tool_installed,
            hooks_installed: false,
            hooks_up_to_date: false,
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
        _dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        let bin = params.binary_path.display();
        Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: format!(
                "Xcode: Automatic installation is not supported. \
                 To enable known_human tracking, add the following to your Xcode scheme's \
                 Pre-action (Product → Scheme → Edit Scheme → Build → Pre-actions):\n\
                 \n\
                 {bin}-xcode-watcher --path \"${{SRCROOT}}\"\n\
                 \n\
                 Or run as a background daemon — see docs for the launchd plist setup.",
            ),
        }])
    }
}

fn is_xcode_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        Path::new("/Applications/Xcode.app").exists()
            || Path::new("/Applications/Xcode-beta.app").exists()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = Path::new("");
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xcode_installer_name() {
        assert_eq!(XcodeInstaller.name(), "Xcode");
    }

    #[test]
    fn test_xcode_installer_id() {
        assert_eq!(XcodeInstaller.id(), "xcode");
    }

    #[test]
    fn test_xcode_install_hooks_returns_none() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        assert!(
            XcodeInstaller
                .install_hooks(&params, false)
                .unwrap()
                .is_none()
        );
    }
}
