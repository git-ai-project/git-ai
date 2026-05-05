use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

use super::opencode::{
    OpenCodeFamilyInstallerConfig, family_plugin_path, generate_family_plugin_content,
};

pub struct KiloInstaller;

static KILO_INSTALLER_CONFIG: OpenCodeFamilyInstallerConfig = OpenCodeFamilyInstallerConfig {
    name: "Kilo Code",
    id: "kilo",
    process_names: &["kilo"],
    config_dir_name: "kilo",
    checkpoint_preset: "kilo",
    plugin_package: "@kilocode/plugin",
};

impl KiloInstaller {
    fn plugin_path() -> PathBuf {
        family_plugin_path(&KILO_INSTALLER_CONFIG)
    }

    pub(crate) fn generate_plugin_content(binary_path: &std::path::Path) -> String {
        generate_family_plugin_content(&KILO_INSTALLER_CONFIG, binary_path)
    }
}

impl HookInstaller for KiloInstaller {
    fn name(&self) -> &str {
        KILO_INSTALLER_CONFIG.name
    }

    fn id(&self) -> &str {
        KILO_INSTALLER_CONFIG.id
    }

    fn process_names(&self) -> Vec<&str> {
        KILO_INSTALLER_CONFIG.process_names.to_vec()
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("kilo");
        let has_global_config = home_dir().join(".config").join("kilo").exists();
        let has_local_config = Path::new(".kilo").exists();

        if !has_binary && !has_global_config && !has_local_config {
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
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_kilo_plugin_content_is_valid_typescript() {
        let binary_path = create_test_binary_path();
        let content = KiloInstaller::generate_plugin_content(&binary_path);

        assert!(content.contains("import type { Plugin }"));
        assert!(content.contains("@kilocode/plugin"));
        assert!(content.contains("export const GitAiPlugin: Plugin"));
        assert!(content.contains("\"tool.execute.before\""));
        assert!(content.contains("\"tool.execute.after\""));
        assert!(content.contains("checkpoint kilo"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(!content.contains("__CHECKPOINT_PRESET__"));
        assert!(!content.contains("__PLUGIN_PACKAGE__"));
    }

    #[test]
    fn test_kilo_plugin_placeholder_substitution() {
        let binary_path = create_test_binary_path();
        let content = KiloInstaller::generate_plugin_content(&binary_path);

        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(content.contains(r#"const GIT_AI_BIN = "/usr/local/bin/git-ai""#));
        assert!(content.contains("${GIT_AI_BIN} --version"));
        assert!(content.contains("${GIT_AI_BIN} checkpoint kilo"));
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
    fn test_kilo_plugin_skips_if_already_exists() {
        let temp_dir = TempDir::new().unwrap();
        let plugin_path = temp_dir.path().join("git-ai.ts");
        let binary_path = create_test_binary_path();

        let content1 = KiloInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &content1).unwrap();

        let content2 = KiloInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &content2).unwrap();

        assert_eq!(content1, content2);
    }

    #[test]
    fn test_kilo_plugin_updates_outdated_content() {
        let temp_dir = TempDir::new().unwrap();
        let plugin_path = temp_dir.path().join("git-ai.ts");
        let binary_path = create_test_binary_path();

        let old_content = "// Old plugin version\nexport const OldPlugin = {}";
        fs::write(&plugin_path, old_content).unwrap();

        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("OldPlugin"));

        let new_content = KiloInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &new_content).unwrap();

        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("GitAiPlugin"));
        assert!(!content.contains("OldPlugin"));
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
        assert!(content.contains("GitAiPlugin"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
    }
}
