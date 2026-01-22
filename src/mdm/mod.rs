pub mod agents;
pub mod hook_installer;
pub mod skills_installer;
pub mod spinner;
pub mod utils;

pub use agents::get_all_installers;
pub use hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult};
pub use skills_installer::{install_skills, uninstall_skills};
