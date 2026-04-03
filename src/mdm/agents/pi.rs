use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

const PI_EXTENSION_CONTENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/pi/git-ai.ts"
));

pub struct PiInstaller;

impl PiInstaller {
    fn extension_path() -> PathBuf {
        home_dir()
            .join(".pi")
            .join("agent")
            .join("extensions")
            .join("git-ai.ts")
    }

    fn generate_extension_content(binary_path: &Path) -> String {
        let path_str = binary_path.display().to_string().replace('\\', "\\\\");
        PI_EXTENSION_CONTENT.replace("__GIT_AI_BINARY_PATH__", &path_str)
    }
}

impl HookInstaller for PiInstaller {
    fn name(&self) -> &str {
        "Pi"
    }

    fn id(&self) -> &str {
        "pi"
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("pi");
        let has_global_config = home_dir().join(".pi").exists();
        let has_local_config = Path::new(".pi").exists();

        if !has_binary && !has_global_config && !has_local_config {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let extension_path = Self::extension_path();
        if !extension_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let current_content = fs::read_to_string(&extension_path).unwrap_or_default();
        let expected_content = Self::generate_extension_content(&params.binary_path);

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed: true,
            hooks_up_to_date: current_content.trim() == expected_content.trim(),
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let extension_path = Self::extension_path();

        if let Some(dir) = extension_path.parent()
            && !dry_run
        {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if extension_path.exists() {
            fs::read_to_string(&extension_path)?
        } else {
            String::new()
        };
        let new_content = Self::generate_extension_content(&params.binary_path);

        if existing_content.trim() == new_content.trim() {
            return Ok(None);
        }

        let diff_output = generate_diff(&extension_path, &existing_content, &new_content);

        if !dry_run {
            if let Some(dir) = extension_path.parent() {
                fs::create_dir_all(dir)?;
            }
            write_atomic(&extension_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let extension_path = Self::extension_path();

        if !extension_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(&extension_path)?;
        let diff_output = generate_diff(&extension_path, &existing_content, "");

        if !dry_run {
            fs::remove_file(&extension_path)?;
        }

        Ok(Some(diff_output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_pi_extension_output_path() {
        assert_eq!(
            PiInstaller::extension_path(),
            home_dir()
                .join(".pi")
                .join("agent")
                .join("extensions")
                .join("git-ai.ts")
        );
    }

    #[test]
    fn test_pi_extension_content_contains_checkpoint_command() {
        let content = PiInstaller::generate_extension_content(&create_test_binary_path());

        assert!(content.contains("'checkpoint', 'pi', '--hook-input', 'stdin'"));
        assert!(content.contains("--hook-input', 'stdin'"));
        assert!(content.contains("hook_event_name"));
        assert!(content.contains("session_path"));
        assert!(content.contains("tool_name_raw"));
        assert!(content.contains("tool_name"));
        assert!(content.contains("will_edit_filepaths"));
        assert!(content.contains("edited_filepaths"));
        assert!(content.contains(r#"const GIT_AI_BIN = '/usr/local/bin/git-ai'"#));
    }

    #[test]
    fn test_pi_extension_content_contains_tool_normalization_table() {
        let content = PiInstaller::generate_extension_content(&create_test_binary_path());

        assert!(content.contains("type ToolOverridePolicy = {"));
        assert!(content.contains("type OverrideConfig = {"));
        assert!(content.contains("version: 1;"));
        assert!(content.contains("tools: Record<string, ToolOverridePolicy>;"));
        assert!(
            content.contains("const DEFAULT_TOOL_POLICIES: Record<string, ToolOverridePolicy> = {")
        );
        assert!(content.contains("edit: {"));
        assert!(content.contains("canonical: 'edit'"));
        assert!(content.contains("write: {"));
        assert!(content.contains("canonical: 'write'"));
        assert!(content.contains("filepath_fields: ['path']"));
        assert!(content.contains("git-ai.override.json"));
        assert!(content.contains("normalizeToolPolicy"));
        assert!(content.contains("override?.tools ?? {}"));
    }
}
