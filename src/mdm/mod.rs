pub mod agents;
pub mod ensure_git_symlinks;
pub mod git_client_installer;
pub mod git_clients;
pub mod hook_installer;
pub mod jetbrains;
pub mod skills_installer;
pub mod spinner;
pub mod utils;

pub use agents::get_all_installers;
pub use ensure_git_symlinks::ensure_git_symlinks;
pub use git_client_installer::{GitClientCheckResult, GitClientInstaller, GitClientInstallerParams};
pub use git_clients::get_all_git_client_installers;
pub use hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult};
pub use skills_installer::{install_skills, uninstall_skills};
