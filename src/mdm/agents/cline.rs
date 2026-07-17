use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{generate_diff, home_dir, normalize_windows_path_for_shell, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

pub struct ClineInstaller;

const MANAGED_MARKER: &str = "# git-ai-managed: cline";
const PRE_HOOK_NAME: &str = "PreToolUse";
const POST_HOOK_NAME: &str = "PostToolUse";
const CLINE_PUBLISHER_ID: &str = "saoudrizwan.claude-dev";

impl ClineInstaller {
    fn storage_paths() -> Vec<PathBuf> {
        if let Ok(test_path) = std::env::var("GIT_AI_CLINE_STORAGE_PATH") {
            return vec![PathBuf::from(test_path)];
        }

        let products = ["Code", "Code - Insiders", "Cursor"];

        #[cfg(target_os = "macos")]
        {
            let base = home_dir().join("Library").join("Application Support");
            products
                .iter()
                .map(|p| {
                    base.join(p)
                        .join("User")
                        .join("globalStorage")
                        .join(CLINE_PUBLISHER_ID)
                })
                .collect()
        }

        #[cfg(target_os = "linux")]
        {
            let base = home_dir().join(".config");
            products
                .iter()
                .map(|p| {
                    base.join(p)
                        .join("User")
                        .join("globalStorage")
                        .join(CLINE_PUBLISHER_ID)
                })
                .collect()
        }

        #[cfg(target_os = "windows")]
        {
            let base = if let Ok(app_data) = std::env::var("APPDATA") {
                PathBuf::from(app_data)
            } else {
                home_dir().join("AppData").join("Roaming")
            };
            products
                .iter()
                .map(|p| {
                    base.join(p)
                        .join("User")
                        .join("globalStorage")
                        .join(CLINE_PUBLISHER_ID)
                })
                .collect()
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            vec![]
        }
    }

    fn hooks_dir() -> PathBuf {
        home_dir().join("Documents").join("Cline").join("Hooks")
    }

    fn hook_path(name: &str) -> PathBuf {
        Self::hooks_dir().join(name)
    }

    fn generate_hook_script(binary_path: &Path) -> String {
        let binary = normalize_windows_path_for_shell(binary_path);
        format!(
            "#!/bin/sh\n{}\n\"{}\" checkpoint cline --hook-input stdin\necho '{{\"cancel\":false}}'\n",
            MANAGED_MARKER, binary
        )
    }

    fn is_managed_script(path: &Path) -> bool {
        fs::read_to_string(path)
            .ok()
            .map(|content| {
                content
                    .lines()
                    .any(|line| line.trim() == MANAGED_MARKER.trim())
            })
            .unwrap_or(false)
    }

    fn is_windows() -> bool {
        cfg!(target_os = "windows")
    }

    fn install_hook_script(
        path: &Path,
        content: &str,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let existing = if path.exists() {
            fs::read_to_string(path)?
        } else {
            String::new()
        };

        if existing.trim() == content.trim() {
            return Ok(None);
        }

        let diff = generate_diff(path, &existing, content);

        if !dry_run {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            write_atomic(path, content.as_bytes())?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
            }
        }

        Ok(Some(diff))
    }

    fn uninstall_hook_script(path: &Path, dry_run: bool) -> Result<Option<String>, GitAiError> {
        if !path.exists() || !Self::is_managed_script(path) {
            return Ok(None);
        }

        let existing = fs::read_to_string(path)?;
        let diff = generate_diff(path, &existing, "");

        if !dry_run {
            fs::remove_file(path)?;
        }

        Ok(Some(diff))
    }
}

impl HookInstaller for ClineInstaller {
    fn name(&self) -> &str {
        "Cline"
    }

    fn id(&self) -> &str {
        "cline"
    }

    fn process_names(&self) -> Vec<&str> {
        vec![]
    }

    fn uses_config_hooks(&self) -> bool {
        true
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed = Self::storage_paths().iter().any(|p| p.exists());

        if Self::is_windows() {
            // Cline hooks are not supported on Windows today; report the tool if it
            // is installed but leave hooks uninstalled.
            return Ok(HookCheckResult {
                tool_installed,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let pre_path = Self::hook_path(PRE_HOOK_NAME);
        let post_path = Self::hook_path(POST_HOOK_NAME);

        let hooks_installed =
            pre_path.exists() && post_path.exists() && Self::is_managed_script(&pre_path);

        let expected = Self::generate_hook_script(&params.binary_path);
        let hooks_up_to_date = if hooks_installed {
            let pre_ok =
                fs::read_to_string(&pre_path).unwrap_or_default().trim() == expected.trim();
            let post_ok =
                fs::read_to_string(&post_path).unwrap_or_default().trim() == expected.trim();
            pre_ok && post_ok
        } else {
            false
        };

        Ok(HookCheckResult {
            tool_installed,
            hooks_installed,
            hooks_up_to_date,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if Self::is_windows() {
            return Ok(None);
        }

        if !dry_run {
            fs::create_dir_all(Self::hooks_dir())?;
        }

        let script = Self::generate_hook_script(&params.binary_path);
        let pre_path = Self::hook_path(PRE_HOOK_NAME);
        let post_path = Self::hook_path(POST_HOOK_NAME);

        let pre_diff = Self::install_hook_script(&pre_path, &script, dry_run)?;
        let post_diff = Self::install_hook_script(&post_path, &script, dry_run)?;

        match (pre_diff, post_diff) {
            (None, None) => Ok(None),
            (Some(a), None) => Ok(Some(a)),
            (None, Some(b)) => Ok(Some(b)),
            (Some(a), Some(b)) => Ok(Some(format!("{}\n{}", a, b))),
        }
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if Self::is_windows() {
            return Ok(None);
        }

        let pre_path = Self::hook_path(PRE_HOOK_NAME);
        let post_path = Self::hook_path(POST_HOOK_NAME);

        let pre_diff = Self::uninstall_hook_script(&pre_path, dry_run)?;
        let post_diff = Self::uninstall_hook_script(&post_path, dry_run)?;

        match (pre_diff, post_diff) {
            (None, None) => Ok(None),
            (Some(a), None) => Ok(Some(a)),
            (None, Some(b)) => Ok(Some(b)),
            (Some(a), Some(b)) => Ok(Some(format!("{}\n{}", a, b))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    fn with_temp_home<F: FnOnce(&Path)>(f: F) {
        let temp_dir = TempDir::new().unwrap();
        let home = temp_dir.path().to_path_buf();

        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        let prev_storage = std::env::var_os("GIT_AI_CLINE_STORAGE_PATH");

        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
        }

        f(&home);

        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
            match prev_storage {
                Some(v) => std::env::set_var("GIT_AI_CLINE_STORAGE_PATH", v),
                None => std::env::remove_var("GIT_AI_CLINE_STORAGE_PATH"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_cline_check_not_installed() {
        with_temp_home(|home| {
            let storage = home.join("cline-storage");
            unsafe { std::env::set_var("GIT_AI_CLINE_STORAGE_PATH", &storage) };

            let params = HookInstallerParams {
                binary_path: create_test_binary_path(),
            };
            let result = ClineInstaller.check_hooks(&params).unwrap();
            assert!(!result.tool_installed);
            assert!(!result.hooks_installed);
            assert!(!result.hooks_up_to_date);
        });
    }

    #[test]
    #[serial]
    fn test_cline_install_creates_hooks() {
        with_temp_home(|home| {
            let storage = home.join("cline-storage");
            fs::create_dir_all(&storage).unwrap();
            unsafe { std::env::set_var("GIT_AI_CLINE_STORAGE_PATH", &storage) };

            let params = HookInstallerParams {
                binary_path: create_test_binary_path(),
            };

            let result = ClineInstaller.install_hooks(&params, false).unwrap();
            assert!(result.is_some(), "expected a diff");

            let pre_path = ClineInstaller::hook_path(PRE_HOOK_NAME);
            let post_path = ClineInstaller::hook_path(POST_HOOK_NAME);

            assert!(pre_path.exists());
            assert!(post_path.exists());

            let content = fs::read_to_string(&pre_path).unwrap();
            assert!(content.contains("git-ai-managed"));
            assert!(content.contains("checkpoint cline"));
            assert!(content.contains(r#"{"cancel":false}"#));

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&pre_path).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "hook should be executable");
            }

            let check = ClineInstaller.check_hooks(&params).unwrap();
            assert!(check.tool_installed);
            assert!(check.hooks_installed);
            assert!(check.hooks_up_to_date);
        });
    }

    #[test]
    #[serial]
    fn test_cline_install_is_idempotent() {
        with_temp_home(|home| {
            let storage = home.join("cline-storage");
            fs::create_dir_all(&storage).unwrap();
            unsafe { std::env::set_var("GIT_AI_CLINE_STORAGE_PATH", &storage) };

            let params = HookInstallerParams {
                binary_path: create_test_binary_path(),
            };

            ClineInstaller.install_hooks(&params, false).unwrap();
            let second = ClineInstaller.install_hooks(&params, false).unwrap();
            assert!(second.is_none(), "second install should be a no-op");
        });
    }

    #[test]
    #[serial]
    fn test_cline_uninstall_removes_hooks() {
        with_temp_home(|home| {
            let storage = home.join("cline-storage");
            fs::create_dir_all(&storage).unwrap();
            unsafe { std::env::set_var("GIT_AI_CLINE_STORAGE_PATH", &storage) };

            let params = HookInstallerParams {
                binary_path: create_test_binary_path(),
            };

            ClineInstaller.install_hooks(&params, false).unwrap();
            ClineInstaller.uninstall_hooks(&params, false).unwrap();

            assert!(!ClineInstaller::hook_path(PRE_HOOK_NAME).exists());
            assert!(!ClineInstaller::hook_path(POST_HOOK_NAME).exists());

            let check = ClineInstaller.check_hooks(&params).unwrap();
            assert!(check.tool_installed);
            assert!(!check.hooks_installed);
        });
    }

    #[test]
    #[serial]
    fn test_cline_uninstall_preserves_unmanaged_files() {
        with_temp_home(|home| {
            let storage = home.join("cline-storage");
            fs::create_dir_all(&storage).unwrap();
            unsafe { std::env::set_var("GIT_AI_CLINE_STORAGE_PATH", &storage) };

            fs::create_dir_all(ClineInstaller::hooks_dir()).unwrap();
            let pre_path = ClineInstaller::hook_path(PRE_HOOK_NAME);
            fs::write(&pre_path, "#!/bin/sh\necho 'user hook'\n").unwrap();

            let params = HookInstallerParams {
                binary_path: create_test_binary_path(),
            };

            let result = ClineInstaller.uninstall_hooks(&params, false).unwrap();
            assert!(result.is_none());
            assert!(pre_path.exists());
        });
    }
}
