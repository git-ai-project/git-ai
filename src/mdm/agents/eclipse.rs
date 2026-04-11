use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult,
};
use crate::mdm::utils::{binary_exists, home_dir};

pub struct EclipseInstaller;

impl HookInstaller for EclipseInstaller {
    fn name(&self) -> &str {
        "Eclipse"
    }
    fn id(&self) -> &str {
        "eclipse"
    }
    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        // Detect Eclipse by binary or by presence of the Eclipse p2 provisioning metadata
        let has_binary = binary_exists("eclipse");
        let has_p2 = home_dir().join(".p2").join("pool").exists();
        // Check common Eclipse install locations
        let has_app = {
            #[cfg(target_os = "macos")]
            {
                std::path::Path::new("/Applications/Eclipse.app").exists()
            }
            #[cfg(not(target_os = "macos"))]
            {
                false
            }
        };

        let tool_installed = has_binary || has_p2 || has_app;
        // Plugin install detection would require scanning the Eclipse plugins dir;
        // for now we report not-installed and show manual instructions.
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
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: concat!(
                "Eclipse: Automatic installation is not supported. ",
                "Install the plugin manually via Help → Install New Software → ",
                "Add update site URL from https://github.com/git-ai-project/git-ai/releases, ",
                "then restart Eclipse."
            )
            .to_string(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eclipse_installer_name() {
        assert_eq!(EclipseInstaller.name(), "Eclipse");
    }

    #[test]
    fn test_eclipse_installer_id() {
        assert_eq!(EclipseInstaller.id(), "eclipse");
    }

    #[test]
    fn test_eclipse_install_hooks_returns_none() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        assert!(
            EclipseInstaller
                .install_hooks(&params, false)
                .unwrap()
                .is_none()
        );
    }
}
