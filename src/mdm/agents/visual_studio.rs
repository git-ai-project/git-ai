use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult,
};

pub struct VisualStudioInstaller;

impl HookInstaller for VisualStudioInstaller {
    fn name(&self) -> &str {
        "Visual Studio"
    }
    fn id(&self) -> &str {
        "visual-studio"
    }
    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed = is_visual_studio_installed();
        let hooks_installed = is_vsix_installed();
        Ok(HookCheckResult {
            tool_installed,
            hooks_installed,
            hooks_up_to_date: hooks_installed,
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
        dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        install_vsix(params, dry_run)
    }

    fn uninstall_extras(
        &self,
        params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<UninstallResult>, GitAiError> {
        Ok(vec![run_vsix_uninstall(params)])
    }
}

fn is_visual_studio_installed() -> bool {
    #[cfg(windows)]
    {
        find_vs_installations().is_some()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn is_vsix_installed() -> bool {
    #[cfg(windows)]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let vs_dir = std::path::PathBuf::from(local_app_data)
                .join("Microsoft")
                .join("VisualStudio");
            if let Ok(entries) = std::fs::read_dir(&vs_dir) {
                for entry in entries.flatten() {
                    let ext_dir = entry.path().join("Extensions");
                    if let Ok(exts) = std::fs::read_dir(&ext_dir) {
                        for ext in exts.flatten() {
                            if ext.path().join("git-ai.vsix").exists()
                                || ext.file_name().to_string_lossy().contains("git-ai")
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn install_vsix(
    params: &HookInstallerParams,
    dry_run: bool,
) -> Result<Vec<InstallResult>, GitAiError> {
    #[cfg(windows)]
    {
        install_vsix_windows(params, dry_run)
    }
    #[cfg(not(windows))]
    {
        let _ = (params, dry_run);
        Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: Only available on Windows".to_string(),
        }])
    }
}

fn run_vsix_uninstall(params: &HookInstallerParams) -> UninstallResult {
    #[cfg(windows)]
    {
        uninstall_vsix_windows(params)
    }
    #[cfg(not(windows))]
    {
        let _ = params;
        UninstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: Only available on Windows".to_string(),
        }
    }
}

#[cfg(windows)]
fn find_vs_installations() -> Option<std::path::PathBuf> {
    // Scan %ProgramFiles%\Microsoft Visual Studio\ for devenv.exe
    let program_files = std::env::var("ProgramFiles").ok()?;
    let vs_root = std::path::PathBuf::from(program_files).join("Microsoft Visual Studio");
    if !vs_root.exists() {
        return None;
    }

    // Look for devenv.exe in year/edition subdirs
    for year in &["2022", "2019"] {
        for edition in &["Enterprise", "Professional", "Community", "BuildTools"] {
            let devenv = vs_root
                .join(year)
                .join(edition)
                .join("Common7")
                .join("IDE")
                .join("devenv.exe");
            if devenv.exists() {
                return Some(vs_root.join(year).join(edition));
            }
        }
    }
    None
}

#[cfg(windows)]
fn find_vsix_installer(vs_root: &std::path::Path) -> Option<std::path::PathBuf> {
    let vsix_installer = vs_root
        .join("Common7")
        .join("IDE")
        .join("VSIXInstaller.exe");
    if vsix_installer.exists() {
        Some(vsix_installer)
    } else {
        None
    }
}

#[cfg(windows)]
fn vsix_path(params: &HookInstallerParams) -> Option<std::path::PathBuf> {
    // Look for VSIX next to the git-ai binary: <bin-dir>/lib/visual-studio/GitAiExtension.vsix
    let bin_dir = params.binary_path.parent()?;
    let vsix = bin_dir
        .join("lib")
        .join("visual-studio")
        .join("GitAiExtension.vsix");
    if vsix.exists() { Some(vsix) } else { None }
}

#[cfg(windows)]
fn install_vsix_windows(
    params: &HookInstallerParams,
    dry_run: bool,
) -> Result<Vec<InstallResult>, GitAiError> {
    let Some(vs_root) = find_vs_installations() else {
        return Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: Not detected. Install Visual Studio 2019 or 2022 first."
                .to_string(),
        }]);
    };

    let Some(vsix_installer) = find_vsix_installer(&vs_root) else {
        return Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: VSIXInstaller.exe not found. Install the extension manually from the Visual Studio Marketplace.".to_string(),
        }]);
    };

    let Some(vsix) = vsix_path(params) else {
        return Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: VSIX package not found. Install the extension manually from https://marketplace.visualstudio.com (search for git-ai).".to_string(),
        }]);
    };

    if dry_run {
        return Ok(vec![InstallResult {
            changed: true,
            diff: None,
            message: format!(
                "Visual Studio: Pending VSIX install via {}",
                vsix_installer.display()
            ),
        }]);
    }

    let output = std::process::Command::new(&vsix_installer)
        .args(["/quiet", "/admin", &vsix.display().to_string()])
        .output()?;

    if output.status.success() {
        Ok(vec![InstallResult {
            changed: true,
            diff: None,
            message: "Visual Studio: Extension installed. Restart Visual Studio to activate."
                .to_string(),
        }])
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: format!(
                "Visual Studio: VSIX install failed: {}. Install manually from the Marketplace.",
                stderr.trim()
            ),
        }])
    }
}

#[cfg(windows)]
fn uninstall_vsix_windows(params: &HookInstallerParams) -> UninstallResult {
    let Some(vs_root) = find_vs_installations() else {
        return UninstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: Not detected".to_string(),
        };
    };
    let Some(vsix_installer) = find_vsix_installer(&vs_root) else {
        return UninstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: VSIXInstaller.exe not found".to_string(),
        };
    };
    let _ = params;
    let output = std::process::Command::new(&vsix_installer)
        .args(["/quiet", "/uninstall:io.gitai.visualstudio"])
        .output();
    match output {
        Ok(o) if o.status.success() => UninstallResult {
            changed: true,
            diff: None,
            message: "Visual Studio: Extension uninstalled".to_string(),
        },
        _ => UninstallResult {
            changed: false,
            diff: None,
            message: "Visual Studio: Extension uninstall failed or was not installed".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visual_studio_installer_name() {
        assert_eq!(VisualStudioInstaller.name(), "Visual Studio");
    }

    #[test]
    fn test_visual_studio_installer_id() {
        assert_eq!(VisualStudioInstaller.id(), "visual-studio");
    }

    #[test]
    fn test_visual_studio_install_hooks_returns_none() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        assert!(
            VisualStudioInstaller
                .install_hooks(&params, false)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn test_install_extras_non_windows_returns_windows_only_message() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        let results = VisualStudioInstaller
            .install_extras(&params, false)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].message.contains("Windows"));
    }
}
