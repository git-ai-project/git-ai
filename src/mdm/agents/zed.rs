use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

pub struct ZedInstaller;

impl ZedInstaller {
    /// Path to Zed's config directory, respecting XDG_CONFIG_HOME
    fn config_dir() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            PathBuf::from(xdg).join("zed")
        } else {
            home_dir().join(".config").join("zed")
        }
    }

    /// Path to Zed's global settings
    fn settings_path() -> PathBuf {
        Self::config_dir().join("settings.json")
    }

    /// Generate the context_servers JSON snippet for Zed settings
    fn generate_context_server_config(binary_path: &Path) -> String {
        let path_str = binary_path.display().to_string();
        format!(
            r#"    "git-ai": {{
      "command": {{
        "path": "{}",
        "args": ["mcp-server"]
      }}
    }}"#,
            path_str
        )
    }

    /// Check if Zed settings already contain git-ai context server config
    fn has_context_server_config(settings_content: &str) -> bool {
        settings_content.contains("\"git-ai\"") && settings_content.contains("mcp-server")
    }
}

impl HookInstaller for ZedInstaller {
    fn name(&self) -> &str {
        "Zed"
    }

    fn id(&self) -> &str {
        "zed"
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let settings_path = Self::settings_path();
        let has_binary = binary_exists("zed");
        let has_config_dir = Self::config_dir().exists();

        // Check if Zed is installed (binary or config directory)
        if !has_binary && !has_config_dir {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // Check if context server is configured in settings
        let has_config = if settings_path.exists() {
            let content = fs::read_to_string(&settings_path).unwrap_or_default();
            Self::has_context_server_config(&content)
        } else {
            false
        };

        if !has_config {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // Check if the config references the correct binary path
        let settings_content = fs::read_to_string(&settings_path).unwrap_or_default();
        let binary_path_str = params.binary_path.display().to_string();
        let is_up_to_date = settings_content.contains(&binary_path_str);

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
        let settings_path = Self::settings_path();

        // Read existing settings or create default
        let existing_content = if settings_path.exists() {
            fs::read_to_string(&settings_path)?
        } else {
            String::from("{}")
        };

        // Check if already configured and up to date
        let binary_path_str = params.binary_path.display().to_string();
        if Self::has_context_server_config(&existing_content)
            && existing_content.contains(&binary_path_str)
        {
            return Ok(None);
        }

        let server_config = Self::generate_context_server_config(&params.binary_path);

        // Generate instruction text for the user since we can't safely modify JSONC
        let instruction = format!(
            "Add the following to your Zed settings (~/.config/zed/settings.json) under \"context_servers\":\n\n{}\n\nAlso copy the rules file to your project:\n  mkdir -p .zed && cp {} .zed/rules",
            server_config,
            concat!(env!("CARGO_MANIFEST_DIR"), "/agent-support/zed/rules")
        );

        if !dry_run {
            // If settings file doesn't exist or doesn't have context_servers, create/update it
            if !settings_path.exists() {
                if let Some(dir) = settings_path.parent() {
                    fs::create_dir_all(dir)?;
                }
                let new_content =
                    format!("{{\n  \"context_servers\": {{\n{}\n  }}\n}}", server_config);
                write_atomic(&settings_path, new_content.as_bytes())?;
            } else if !Self::has_context_server_config(&existing_content) {
                // Settings exist but no git-ai config - print instructions
                // We don't modify existing JSONC files as they may have comments
                eprintln!("\n{}\n", instruction);
            } else {
                // Has old config, needs update - print instructions
                eprintln!(
                    "\nUpdate your Zed settings with the new binary path:\n{}\n",
                    server_config
                );
            }
        }

        Ok(Some(instruction))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let settings_path = Self::settings_path();

        if !settings_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&settings_path)?;
        if !Self::has_context_server_config(&content) {
            return Ok(None);
        }

        let instruction =
            "Remove the \"git-ai\" entry from \"context_servers\" in your Zed settings."
                .to_string();

        if !dry_run {
            eprintln!("\n{}\n", instruction);
        }

        Ok(Some(instruction))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zed_rules_file_exists() {
        let rules_path = concat!(env!("CARGO_MANIFEST_DIR"), "/agent-support/zed/rules");
        let content = std::fs::read_to_string(rules_path).expect("rules file should exist");
        assert!(content.contains("git_ai_checkpoint"));
        assert!(content.contains("PreToolUse"));
        assert!(content.contains("PostToolUse"));
        assert!(content.contains("file_paths"));
    }

    #[test]
    fn test_zed_context_server_config() {
        let binary_path = PathBuf::from("/usr/local/bin/git-ai");
        let config = ZedInstaller::generate_context_server_config(&binary_path);
        assert!(config.contains("\"git-ai\""));
        assert!(config.contains("mcp-server"));
        assert!(config.contains("/usr/local/bin/git-ai"));
    }

    #[test]
    fn test_zed_has_context_server_config() {
        let settings_with_config = r#"{
            "context_servers": {
                "git-ai": {
                    "command": {
                        "path": "/usr/local/bin/git-ai",
                        "args": ["mcp-server"]
                    }
                }
            }
        }"#;
        assert!(ZedInstaller::has_context_server_config(
            settings_with_config
        ));

        let settings_without_config = r#"{ "theme": "One Dark" }"#;
        assert!(!ZedInstaller::has_context_server_config(
            settings_without_config
        ));
    }
}
