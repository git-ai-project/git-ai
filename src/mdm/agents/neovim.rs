use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult,
};
use crate::mdm::utils::binary_exists;

pub struct NeovimInstaller;

impl HookInstaller for NeovimInstaller {
    fn name(&self) -> &str {
        "Neovim"
    }
    fn id(&self) -> &str {
        "neovim"
    }
    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed = binary_exists("nvim");
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
                "Neovim: Automatic installation is not supported. ",
                "Install the plugin using one of these methods:\n",
                "\n",
                "  lazy.nvim (recommended):\n",
                "    { 'git-ai-project/git-ai', opts = {} }\n",
                "\n",
                "  packer.nvim:\n",
                "    use { 'git-ai-project/git-ai',\n",
                "          config = function() require('git-ai').setup() end }\n",
                "\n",
                "  Native (~/.config/nvim/init.lua):\n",
                "    vim.opt.rtp:prepend('/path/to/agent-support/neovim')\n",
                "    require('git-ai').setup()",
            )
            .to_string(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_neovim_installer_name() {
        assert_eq!(NeovimInstaller.name(), "Neovim");
    }

    #[test]
    fn test_neovim_installer_id() {
        assert_eq!(NeovimInstaller.id(), "neovim");
    }

    #[test]
    fn test_neovim_install_hooks_returns_none() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        assert!(
            NeovimInstaller
                .install_hooks(&params, false)
                .unwrap()
                .is_none()
        );
    }
}
