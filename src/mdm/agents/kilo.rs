use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

const KILO_PLUGIN_CONTENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/kilo/git-ai.ts"
));

pub struct KiloInstaller;

impl KiloInstaller {
    fn plugin_path() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            home_dir()
                .join("Library")
                .join("Application Support")
                .join("kilo")
                .join("plugins")
                .join("git-ai.ts")
        }
        #[cfg(target_os = "linux")]
        {
            let base = if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
                PathBuf::from(xdg_config)
            } else {
                home_dir().join(".config")
            };
            base.join("kilo").join("plugins").join("git-ai.ts")
        }
        #[cfg(target_os = "windows")]
        {
            let base = if let Ok(app_data) = std::env::var("APPDATA") {
                PathBuf::from(app_data)
            } else {
                home_dir().join("AppData").join("Roaming")
            };
            base.join("kilo").join("plugins").join("git-ai.ts")
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            home_dir()
                .join(".config")
                .join("kilo")
                .join("plugins")
                .join("git-ai.ts")
        }
    }

    fn generate_plugin_content(binary_path: &Path) -> String {
        let path_str = binary_path.display().to_string().replace('\\', "\\\\");
        KILO_PLUGIN_CONTENT.replace("__GIT_AI_BINARY_PATH__", &path_str)
    }
}

impl HookInstaller for KiloInstaller {
    fn name(&self) -> &str {
        "Kilo"
    }

    fn id(&self) -> &str {
        "kilo"
    }

    fn process_names(&self) -> Vec<&str> {
        vec!["kilo", "kilocode"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("kilo") || binary_exists("kilocode");
        let has_global_config = Self::plugin_path()
            .parent()
            .and_then(|p| p.parent())
            .map_or(false, |p| p.exists());

        if !has_binary && !has_global_config {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let plugin_path = Self::plugin_path();
        if !plugin_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let current_content = fs::read_to_string(&plugin_path).unwrap_or_default();
        let expected_content = Self::generate_plugin_content(&params.binary_path);
        let is_up_to_date = current_content.trim() == expected_content.trim();

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: true,
            hooks_up_to_date: is_up_to_date,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let plugin_path = Self::plugin_path();

        if let Some(dir) = plugin_path.parent()
            && !dry_run
        {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if plugin_path.exists() {
            fs::read_to_string(&plugin_path)?
        } else {
            String::new()
        };

        let new_content = Self::generate_plugin_content(&params.binary_path);

        if existing_content.trim() == new_content.trim() {
            return Ok(None);
        }

        let diff_output = generate_diff(&plugin_path, &existing_content, &new_content);

        if !dry_run {
            if let Some(dir) = plugin_path.parent() {
                fs::create_dir_all(dir)?;
            }
            write_atomic(&plugin_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let plugin_path = Self::plugin_path();

        if !plugin_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(&plugin_path)?;
        let diff_output = generate_diff(&plugin_path, &existing_content, "");

        if !dry_run {
            fs::remove_file(&plugin_path)?;
        }

        Ok(Some(diff_output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let plugin_path = temp_dir
            .path()
            .join(".config")
            .join("kilo")
            .join("plugins")
            .join("git-ai.ts");
        (temp_dir, plugin_path)
    }

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_kilo_install_plugin_creates_file_from_scratch() {
        let (_temp_dir, plugin_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let generated = KiloInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &generated).unwrap();

        assert!(plugin_path.exists());

        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("tool.execute.before"));
        assert!(content.contains("tool.execute.after"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
    }

    #[test]
    fn test_kilo_plugin_content_is_valid_typescript() {
        let content = KILO_PLUGIN_CONTENT;

        assert!(content.contains("import type { Plugin }"));
        assert!(content.contains("@kilocode/plugin"));
        assert!(content.contains("tool.execute.before"));
        assert!(content.contains("tool.execute.after"));
        assert!(content.contains("FILE_EDIT_TOOLS"));
        assert!(content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(content.contains("hook_event_name"));
        assert!(content.contains("session_id"));
        assert!(content.contains("PreToolUse"));
        assert!(content.contains("PostToolUse"));
    }

    #[test]
    fn test_kilo_plugin_placeholder_substitution() {
        let binary_path = create_test_binary_path();
        let content = KiloInstaller::generate_plugin_content(&binary_path);

        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(content.contains(r#"const GIT_AI_BIN = "/usr/local/bin/git-ai""#));
        assert!(content.contains("${GIT_AI_BIN} checkpoint kilo"));
    }

    #[test]
    fn test_kilo_plugin_skips_if_already_exists() {
        let (_temp_dir, plugin_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let generated = KiloInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &generated).unwrap();
        let content1 = fs::read_to_string(&plugin_path).unwrap();

        fs::write(&plugin_path, &generated).unwrap();
        let content2 = fs::read_to_string(&plugin_path).unwrap();

        assert_eq!(content1, content2);
    }

    #[test]
    fn test_kilo_plugin_updates_outdated_content() {
        let (_temp_dir, plugin_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let old_content = "// OldPlugin version";
        fs::write(&plugin_path, old_content).unwrap();

        let content_before = fs::read_to_string(&plugin_path).unwrap();
        assert!(content_before.contains("OldPlugin"));

        let generated = KiloInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &generated).unwrap();

        let content_after = fs::read_to_string(&plugin_path).unwrap();
        assert!(content_after.contains("tool.execute.before"));
        assert!(!content_after.contains("OldPlugin"));
    }

    #[test]
    fn test_kilo_plugin_windows_path_escaping() {
        let binary_path = PathBuf::from(r"C:\Users\foo\.git-ai\bin\git-ai.exe");
        let content = KiloInstaller::generate_plugin_content(&binary_path);

        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(
            content.contains(r#"const GIT_AI_BIN = "C:\\Users\\foo\\.git-ai\\bin\\git-ai.exe""#)
        );
    }

    #[test]
    fn test_kilo_plugin_handles_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let binary_path = create_test_binary_path();
        let plugin_path = temp_dir
            .path()
            .join(".config")
            .join("kilo")
            .join("plugins")
            .join("git-ai.ts");

        assert!(!plugin_path.parent().unwrap().exists());

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let generated = KiloInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &generated).unwrap();

        assert!(plugin_path.exists());
        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("tool.execute.before"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
    }
}
