use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult,
};
use crate::mdm::utils::{binary_exists, home_dir};
use std::fs;
use std::path::PathBuf;

pub struct ZedInstaller;

impl ZedInstaller {
    /// Path to Zed's settings.json
    fn settings_path() -> PathBuf {
        home_dir().join(".config").join("zed").join("settings.json")
    }

    /// Check if Zed is installed
    fn is_zed_installed() -> bool {
        binary_exists("zed") || home_dir().join(".config").join("zed").exists()
    }

    /// Check if git-ai acp-proxy is configured in Zed's agent_servers
    fn is_acp_proxy_configured() -> bool {
        let settings_path = Self::settings_path();
        if !settings_path.exists() {
            return false;
        }
        match fs::read_to_string(&settings_path) {
            Ok(content) => content.contains("\"acp-proxy\""),
            Err(_) => false,
        }
    }
}

impl HookInstaller for ZedInstaller {
    fn name(&self) -> &str {
        "Zed"
    }

    fn id(&self) -> &str {
        "zed"
    }

    /// Zed uses ACP agent_servers config, not traditional config file hooks
    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        if !Self::is_zed_installed() {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let configured = Self::is_acp_proxy_configured();

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: configured,
            hooks_up_to_date: configured,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if Self::is_acp_proxy_configured() {
            return Ok(None); // Already configured
        }

        let binary_path = params.binary_path.display().to_string();

        // We can't safely auto-modify Zed's settings.json (it's JSONC with comments),
        // so we print instructions for the user.
        let instructions = format!(
            r#"
To enable git-ai in Zed, add the following to your Zed settings.json
(~/.config/zed/settings.json) inside the top-level object:

  "agent_servers": {{
    "Claude Code (git-ai)": {{
      "type": "custom",
      "command": "{}",
      "args": ["acp-proxy", "--", "claude"]
    }}
  }}

Replace "claude" with your preferred agent command if different.
"#,
            binary_path
        );

        Ok(Some(instructions))
    }

    fn install_extras(
        &self,
        params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        if !Self::is_zed_installed() {
            return Ok(vec![]);
        }

        if Self::is_acp_proxy_configured() {
            return Ok(vec![InstallResult {
                changed: false,
                diff: None,
                message: "Zed: ACP proxy already configured".to_string(),
            }]);
        }

        let binary_path = params.binary_path.display().to_string();
        let instructions = format!(
            r#"Zed: Add the following to ~/.config/zed/settings.json:

  "agent_servers": {{
    "Claude Code (git-ai)": {{
      "type": "custom",
      "command": "{}",
      "args": ["acp-proxy", "--", "claude"]
    }}
  }}"#,
            binary_path
        );

        Ok(vec![InstallResult {
            changed: false,
            diff: None,
            message: instructions,
        }])
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if !Self::is_acp_proxy_configured() {
            return Ok(None);
        }

        let instructions = r#"
To remove git-ai from Zed, delete the "agent_servers" entry containing
"acp-proxy" from your Zed settings.json (~/.config/zed/settings.json).
"#
        .to_string();

        Ok(Some(instructions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zed_installer_name_and_id() {
        let installer = ZedInstaller;
        assert_eq!(installer.name(), "Zed");
        assert_eq!(installer.id(), "zed");
    }

    #[test]
    fn test_zed_installer_does_not_use_config_hooks() {
        let installer = ZedInstaller;
        assert!(!installer.uses_config_hooks());
    }

    #[test]
    fn test_zed_installer_install_prints_instructions() {
        let installer = ZedInstaller;
        let params = HookInstallerParams {
            binary_path: PathBuf::from("/usr/local/bin/git-ai"),
        };

        let result = installer.install_hooks(&params, false).unwrap();

        // If Zed isn't installed on the test machine and acp-proxy isn't configured,
        // it should still return instructions
        if let Some(instructions) = result {
            assert!(instructions.contains("agent_servers"));
            assert!(instructions.contains("acp-proxy"));
            assert!(instructions.contains("/usr/local/bin/git-ai"));
        }
    }

    #[test]
    fn test_zed_settings_path() {
        let path = ZedInstaller::settings_path();
        assert!(path.ends_with("zed/settings.json"));
    }
}
