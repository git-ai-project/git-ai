use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult,
};
use crate::mdm::utils::binary_exists;

pub struct VimInstaller;

impl HookInstaller for VimInstaller {
    fn name(&self) -> &str { "Vim" }
    fn id(&self) -> &str { "vim" }
    fn uses_config_hooks(&self) -> bool { false }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed = binary_exists("vim") || binary_exists("gvim");
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
                "Vim: Automatic installation is not supported. ",
                "Install the plugin using one of these methods:\n",
                "\n",
                "  Native packages (Vim 8+):\n",
                "    mkdir -p ~/.vim/pack/git-ai/start/git-ai\n",
                "    cp -r <repo>/agent-support/vim/* ~/.vim/pack/git-ai/start/git-ai/\n",
                "\n",
                "  vim-plug (~/.vimrc):\n",
                "    Plug 'git-ai-project/git-ai', {'rtp': 'agent-support/vim'}\n",
                "\n",
                "  Vundle (~/.vimrc):\n",
                "    Plugin 'git-ai-project/git-ai'\n",
                "    (also add: set rtp+=~/.vim/bundle/git-ai/agent-support/vim)",
            ).to_string(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vim_installer_name() {
        assert_eq!(VimInstaller.name(), "Vim");
    }

    #[test]
    fn test_vim_installer_id() {
        assert_eq!(VimInstaller.id(), "vim");
    }

    #[test]
    fn test_vim_install_hooks_returns_none() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        assert!(VimInstaller.install_hooks(&params, false).unwrap().is_none());
    }
}
