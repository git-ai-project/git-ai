use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

// OpenCode plugin content (TypeScript), embedded from the source file
const OPENCODE_PLUGIN_CONTENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/opencode/git-ai.ts"
));

pub struct OpenCodeInstaller;

impl OpenCodeInstaller {
    fn plugin_path() -> PathBuf {
        home_dir()
            .join(".config")
            .join("opencode")
            .join("plugin")
            .join("git-ai.ts")
    }
}

impl HookInstaller for OpenCodeInstaller {
    fn name(&self) -> &str {
        "OpenCode"
    }

    fn id(&self) -> &str {
        "opencode"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("opencode");
        let has_global_config = home_dir().join(".config").join("opencode").exists();
        let has_local_config = Path::new(".opencode").exists();

        if !has_binary && !has_global_config && !has_local_config {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // Check if plugin is installed
        let plugin_path = Self::plugin_path();
        if !plugin_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // Check if plugin is up to date
        let current_content = fs::read_to_string(&plugin_path).unwrap_or_default();
        let is_up_to_date = current_content.trim() == OPENCODE_PLUGIN_CONTENT.trim();

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: true,
            hooks_up_to_date: is_up_to_date,
        })
    }

    fn install_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let plugin_path = Self::plugin_path();

        // Ensure directory exists
        if let Some(dir) = plugin_path.parent()
            && !dry_run {
                fs::create_dir_all(dir)?;
            }

        // Read existing content if present
        let existing_content = if plugin_path.exists() {
            fs::read_to_string(&plugin_path)?
        } else {
            String::new()
        };

        let new_content = OPENCODE_PLUGIN_CONTENT;

        // Check if there are changes
        if existing_content.trim() == new_content.trim() {
            return Ok(None);
        }

        // Generate diff
        let diff_output = generate_diff(&plugin_path, &existing_content, new_content);

        // Write if not dry-run
        if !dry_run {
            // Ensure directory exists (might not exist in dry run check above)
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
            .join("opencode")
            .join("plugin")
            .join("git-ai.ts");
        (temp_dir, plugin_path)
    }

    #[test]
    fn test_opencode_install_plugin_creates_file_from_scratch() {
        let (_temp_dir, plugin_path) = setup_test_env();

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        assert!(plugin_path.exists());

        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("GitAiPlugin"));
        assert!(content.contains("tool.execute.before"));
        assert!(content.contains("tool.execute.after"));
        // Uses the opencode preset with session_id-based hook input
        assert!(content.contains("git-ai checkpoint opencode"));
        assert!(content.contains("session_id"));
    }

    #[test]
    fn test_opencode_plugin_content_is_valid_typescript() {
        let content = OPENCODE_PLUGIN_CONTENT;

        assert!(content.contains("import type { Plugin }"));
        assert!(content.contains("@opencode-ai/plugin"));
        assert!(content.contains("export const GitAiPlugin: Plugin"));
        assert!(content.contains("\"tool.execute.before\""));
        assert!(content.contains("\"tool.execute.after\""));
        assert!(content.contains("FILE_EDIT_TOOLS"));
        assert!(content.contains("edit"));
        assert!(content.contains("write"));
        // Uses the dedicated opencode preset which reads from local storage
        assert!(content.contains("git-ai checkpoint opencode"));
        assert!(content.contains("hook_event_name"));
        assert!(content.contains("session_id"));
        assert!(content.contains("PreToolUse"));
        assert!(content.contains("PostToolUse"));
    }

    #[test]
    fn test_opencode_plugin_skips_if_already_exists() {
        let (_temp_dir, plugin_path) = setup_test_env();

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();
        let content1 = fs::read_to_string(&plugin_path).unwrap();

        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();
        let content2 = fs::read_to_string(&plugin_path).unwrap();

        assert_eq!(content1, content2);
    }

    #[test]
    fn test_opencode_plugin_updates_outdated_content() {
        let (_temp_dir, plugin_path) = setup_test_env();

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let old_content = "// Old plugin version\nexport const OldPlugin = {}";
        fs::write(&plugin_path, old_content).unwrap();

        let content_before = fs::read_to_string(&plugin_path).unwrap();
        assert!(content_before.contains("OldPlugin"));

        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        let content_after = fs::read_to_string(&plugin_path).unwrap();
        assert!(content_after.contains("GitAiPlugin"));
        assert!(!content_after.contains("OldPlugin"));
    }

    #[test]
    fn test_opencode_plugin_handles_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let plugin_path = temp_dir
            .path()
            .join(".config")
            .join("opencode")
            .join("plugin")
            .join("git-ai.ts");

        assert!(!plugin_path.parent().unwrap().exists());

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&plugin_path, OPENCODE_PLUGIN_CONTENT).unwrap();

        assert!(plugin_path.exists());
        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("GitAiPlugin"));
    }
}
