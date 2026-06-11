use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult,
};

pub struct NotepadPlusPlusInstaller;

impl HookInstaller for NotepadPlusPlusInstaller {
    fn name(&self) -> &str {
        "Notepad++"
    }

    fn id(&self) -> &str {
        "notepad-plus-plus"
    }

    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed = is_notepad_plus_plus_installed();
        let hooks_installed = is_plugin_installed();
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

    fn process_names(&self) -> Vec<&str> {
        #[cfg(windows)]
        {
            vec!["notepad++"]
        }
        #[cfg(not(windows))]
        {
            vec![]
        }
    }

    fn install_extras(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        install_plugin(params, dry_run)
    }

    fn uninstall_extras(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<UninstallResult>, GitAiError> {
        Ok(vec![uninstall_plugin()])
    }
}

fn is_notepad_plus_plus_installed() -> bool {
    #[cfg(windows)]
    {
        notepad_exe_path().is_some()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn is_plugin_installed() -> bool {
    #[cfg(windows)]
    {
        plugin_dest_path().map(|p| p.exists()).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn install_plugin(
    params: &HookInstallerParams,
    dry_run: bool,
) -> Result<Vec<InstallResult>, GitAiError> {
    #[cfg(windows)]
    {
        install_plugin_windows(params, dry_run)
    }
    #[cfg(not(windows))]
    {
        let _ = (params, dry_run);
        Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Notepad++: Only available on Windows".to_string(),
        }])
    }
}

fn uninstall_plugin() -> UninstallResult {
    #[cfg(windows)]
    {
        uninstall_plugin_windows()
    }
    #[cfg(not(windows))]
    {
        UninstallResult {
            changed: false,
            diff: None,
            message: "Notepad++: Only available on Windows".to_string(),
        }
    }
}

#[cfg(windows)]
fn notepad_exe_path() -> Option<std::path::PathBuf> {
    // Check common install locations
    let candidates = [
        std::env::var("ProgramFiles").ok().map(|p| {
            std::path::PathBuf::from(p)
                .join("Notepad++")
                .join("notepad++.exe")
        }),
        std::env::var("ProgramFiles(x86)").ok().map(|p| {
            std::path::PathBuf::from(p)
                .join("Notepad++")
                .join("notepad++.exe")
        }),
        std::env::var("LOCALAPPDATA").ok().map(|p| {
            std::path::PathBuf::from(p)
                .join("Programs")
                .join("Notepad++")
                .join("notepad++.exe")
        }),
    ];
    candidates.into_iter().flatten().find(|c| c.exists())
}

#[cfg(windows)]
fn plugin_dest_path() -> Option<std::path::PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(
        std::path::PathBuf::from(appdata)
            .join("Notepad++")
            .join("plugins")
            .join("git-ai")
            .join("git-ai.dll"),
    )
}

#[cfg(windows)]
fn dll_source_path(params: &HookInstallerParams) -> Option<std::path::PathBuf> {
    // Look for the DLL next to the git-ai binary: <bin-dir>/lib/notepad-plus-plus/git-ai.dll
    let bin_dir = params.binary_path.parent()?;
    let dll = bin_dir
        .join("lib")
        .join("notepad-plus-plus")
        .join("git-ai.dll");
    if dll.exists() { Some(dll) } else { None }
}

#[cfg(windows)]
fn install_plugin_windows(
    params: &HookInstallerParams,
    dry_run: bool,
) -> Result<Vec<InstallResult>, GitAiError> {
    if notepad_exe_path().is_none() {
        return Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Notepad++: Not detected. Install Notepad++ first.".to_string(),
        }]);
    }

    let Some(dest) = plugin_dest_path() else {
        return Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Notepad++: Could not determine plugin directory".to_string(),
        }]);
    };

    if dest.exists() {
        return Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: "Notepad++: Plugin already installed".to_string(),
        }]);
    }

    let Some(src) = dll_source_path(params) else {
        return Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: concat!(
                "Notepad++: Plugin DLL not found. Install manually: ",
                "download git-ai.dll and copy it to ",
                "%APPDATA%\\Notepad++\\plugins\\git-ai\\git-ai.dll, ",
                "then restart Notepad++."
            )
            .to_string(),
        }]);
    };

    if dry_run {
        return Ok(vec![InstallResult {
            changed: true,
            diff: None,
            message: format!("Notepad++: Pending plugin install to {}", dest.display()),
        }]);
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&src, &dest)?;

    Ok(vec![InstallResult {
        changed: true,
        diff: None,
        message: format!(
            "Notepad++: Plugin installed to {}. Restart Notepad++ to activate.",
            dest.display()
        ),
    }])
}

#[cfg(windows)]
fn uninstall_plugin_windows() -> UninstallResult {
    let Some(dest) = plugin_dest_path() else {
        return UninstallResult {
            changed: false,
            diff: None,
            message: "Notepad++: Could not determine plugin directory".to_string(),
        };
    };
    if let Some(dir) = dest.parent()
        && dir.exists()
        && std::fs::remove_dir_all(dir).is_ok()
    {
        return UninstallResult {
            changed: true,
            diff: None,
            message: "Notepad++: Plugin removed".to_string(),
        };
    }
    UninstallResult {
        changed: false,
        diff: None,
        message: "Notepad++: Plugin was not installed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notepad_plus_plus_installer_name() {
        assert_eq!(NotepadPlusPlusInstaller.name(), "Notepad++");
    }

    #[test]
    fn test_notepad_plus_plus_installer_id() {
        assert_eq!(NotepadPlusPlusInstaller.id(), "notepad-plus-plus");
    }

    #[test]
    fn test_notepad_install_hooks_returns_none() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        assert!(
            NotepadPlusPlusInstaller
                .install_hooks(&params, false)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn test_install_extras_non_windows_message() {
        let params = HookInstallerParams {
            binary_path: std::path::PathBuf::from("/usr/local/bin/git-ai"),
        };
        let results = NotepadPlusPlusInstaller
            .install_extras(&params, false)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].message.contains("Windows"));
    }
}
