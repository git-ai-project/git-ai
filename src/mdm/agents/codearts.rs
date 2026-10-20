use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

// CodeArts exposes the same tool lifecycle hooks as OpenCode.
const PLUGIN_CONTENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/opencode/git-ai.ts"
));

pub struct CodeArtsInstaller {
    config_dir: PathBuf,
}

impl Default for CodeArtsInstaller {
    fn default() -> Self {
        Self {
            config_dir: home_dir().join(".codeartsdoer"),
        }
    }
}

impl CodeArtsInstaller {
    fn plugin_path(&self) -> PathBuf {
        self.config_dir.join("plugins").join("git-ai.ts")
    }

    fn generate_plugin_content(binary_path: &Path) -> String {
        let path_literal = serde_json::to_string(&binary_path.to_string_lossy())
            .expect("serializing a binary path string cannot fail");
        PLUGIN_CONTENT
            .replace(
                r#"const AGENT_NAME = "opencode""#,
                r#"const AGENT_NAME = "codearts""#,
            )
            .replace("OpenCode", "CodeArts")
            .replace(".config/opencode/plugins", ".codeartsdoer/plugins")
            .replace(".opencode/plugins", ".codeartsdoer/plugins")
            .replace(
                "https://opencode.ai/docs/plugins/",
                "https://support.huaweicloud.com/usermanual-cli/codeartsagent_cli_0018.html",
            )
            // Substitute last so template branding cannot rewrite the binary path.
            .replace(r#""__GIT_AI_BINARY_PATH__""#, &path_literal)
    }
}

impl HookInstaller for CodeArtsInstaller {
    fn name(&self) -> &str {
        "CodeArts"
    }

    fn id(&self) -> &str {
        "codearts"
    }

    fn process_names(&self) -> Vec<&str> {
        vec!["codearts"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed = self.config_dir.exists()
            || Path::new(".codeartsdoer").exists()
            || binary_exists("codearts");
        let plugin_path = self.plugin_path();
        let hooks_installed = tool_installed && plugin_path.is_file();
        let hooks_up_to_date = hooks_installed
            && fs::read_to_string(&plugin_path)?.trim()
                == Self::generate_plugin_content(&params.binary_path).trim();

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
        let plugin_path = self.plugin_path();
        let existing_content = if plugin_path.exists() {
            fs::read_to_string(&plugin_path)?
        } else {
            String::new()
        };
        let new_content = Self::generate_plugin_content(&params.binary_path);
        if existing_content.trim() == new_content.trim() {
            return Ok(None);
        }

        let diff = generate_diff(&plugin_path, &existing_content, &new_content);
        if !dry_run {
            write_atomic(&plugin_path, new_content.as_bytes())?;
        }
        Ok(Some(diff))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let plugin_path = self.plugin_path();
        if !plugin_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(&plugin_path)?;
        let diff = generate_diff(&plugin_path, &existing_content, "");
        if !dry_run {
            fs::remove_file(&plugin_path)?;
        }
        Ok(Some(diff))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn installer_at(temp: &TempDir) -> CodeArtsInstaller {
        CodeArtsInstaller {
            config_dir: temp.path().join(".codeartsdoer"),
        }
    }

    fn params() -> HookInstallerParams {
        HookInstallerParams {
            binary_path: PathBuf::from("/usr/local/bin/git-ai"),
        }
    }

    #[test]
    fn test_codearts_installer_is_registered_once() {
        let installers = crate::mdm::agents::get_all_installers();
        let codearts: Vec<_> = installers
            .iter()
            .filter(|item| item.id() == "codearts")
            .collect();
        assert_eq!(codearts.len(), 1);
        assert_eq!(codearts[0].name(), "CodeArts");
    }

    #[test]
    fn test_codearts_install_dry_run_has_no_side_effects() {
        let temp = TempDir::new().unwrap();
        let installer = installer_at(&temp);

        let diff = installer.install_hooks(&params(), true).unwrap().unwrap();

        assert!(diff.contains("git-ai.ts"));
        assert!(diff.contains("const AGENT_NAME = \"codearts\""));
        assert!(!installer.config_dir.exists());
    }

    #[test]
    fn test_codearts_install_update_and_uninstall_lifecycle() {
        let temp = TempDir::new().unwrap();
        let installer = installer_at(&temp);
        let params = params();
        fs::create_dir_all(&installer.config_dir).unwrap();

        let before = installer.check_hooks(&params).unwrap();
        assert!(before.tool_installed);
        assert!(!before.hooks_installed);
        assert!(!before.hooks_up_to_date);

        assert!(installer.install_hooks(&params, false).unwrap().is_some());
        let plugin_path = installer.config_dir.join("plugins").join("git-ai.ts");
        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("const AGENT_NAME = \"codearts\""));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        let installed = installer.check_hooks(&params).unwrap();
        assert!(installed.tool_installed);
        assert!(installed.hooks_installed);
        assert!(installed.hooks_up_to_date);
        assert!(installer.install_hooks(&params, false).unwrap().is_none());

        fs::write(&plugin_path, "// outdated git-ai plugin").unwrap();
        let outdated = installer.check_hooks(&params).unwrap();
        assert!(outdated.hooks_installed);
        assert!(!outdated.hooks_up_to_date);
        assert!(installer.install_hooks(&params, false).unwrap().is_some());
        assert_eq!(fs::read_to_string(&plugin_path).unwrap(), content);

        let other_plugin = plugin_path.with_file_name("custom.ts");
        fs::write(&other_plugin, "// user plugin").unwrap();
        assert!(installer.uninstall_hooks(&params, true).unwrap().is_some());
        assert_eq!(fs::read_to_string(&plugin_path).unwrap(), content);
        assert!(installer.uninstall_hooks(&params, false).unwrap().is_some());
        assert!(!plugin_path.exists());
        assert_eq!(fs::read_to_string(&other_plugin).unwrap(), "// user plugin");
        assert!(installer.uninstall_hooks(&params, false).unwrap().is_none());
        let uninstalled = installer.check_hooks(&params).unwrap();
        assert!(uninstalled.tool_installed);
        assert!(!uninstalled.hooks_installed);
        assert!(!uninstalled.hooks_up_to_date);
    }

    #[test]
    fn test_codearts_binary_path_is_a_valid_string_literal() {
        for raw_path in [
            "C:\\Users\\a\"b\\tools\\git-ai.exe",
            "C:\\Tools\\OpenCode\\git-ai.exe",
            "/opt/.config/opencode/plugins/git-ai",
        ] {
            let path = PathBuf::from(raw_path);
            let content = CodeArtsInstaller::generate_plugin_content(&path);
            let literal = content
                .lines()
                .find_map(|line| line.strip_prefix("const GIT_AI_BIN = "))
                .unwrap();

            assert_eq!(
                serde_json::from_str::<String>(literal).unwrap(),
                path.to_string_lossy()
            );
            assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        }
    }
}
