use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

// Amp plugin content (TypeScript), embedded from the source file
const AMP_PLUGIN_CONTENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/amp/git-ai.ts"
));

pub struct AmpInstaller;

impl AmpInstaller {
    fn plugin_path() -> PathBuf {
        home_dir()
            .join(".config")
            .join("amp")
            .join("plugins")
            .join("git-ai.ts")
    }

    /// Generate plugin content with the absolute binary path substituted in.
    fn generate_plugin_content(binary_path: &Path) -> String {
        // Escape backslashes for TypeScript string literals (needed for Windows paths)
        let path_str = binary_path.display().to_string().replace('\\', "\\\\");
        AMP_PLUGIN_CONTENT.replace("__GIT_AI_BINARY_PATH__", &path_str)
    }
}

impl HookInstaller for AmpInstaller {
    fn name(&self) -> &str {
        "Amp"
    }

    fn id(&self) -> &str {
        "amp"
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("amp");
        let has_global_config = home_dir().join(".config").join("amp").exists();
        let has_local_config = Path::new(".amp").exists();

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
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let plugin_path = temp_dir
            .path()
            .join(".config")
            .join("amp")
            .join("plugins")
            .join("git-ai.ts");
        (temp_dir, plugin_path)
    }

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_amp_install_plugin_creates_file_from_scratch() {
        let (_temp_dir, plugin_path) = setup_test_env();
        let binary_path = create_test_binary_path();

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let generated = AmpInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &generated).unwrap();

        assert!(plugin_path.exists());

        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("hook_event_name"));
        assert!(content.contains("PreToolUse"));
        assert!(content.contains("PostToolUse"));
        assert!(content.contains("checkpoint amp"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(content.contains(r#"const GIT_AI_BIN = '/usr/local/bin/git-ai'"#));
    }

    #[test]
    fn test_amp_plugin_content_is_valid_typescript() {
        let content = AMP_PLUGIN_CONTENT;

        assert!(
            content
                .lines()
                .next()
                .unwrap_or_default()
                .contains("@i-know-the-amp-plugin-api-is-wip-and-very-experimental-right-now")
        );
        assert!(
            content.contains("import type { PluginAPI, ToolCallEvent } from '@ampcode/plugin'")
        );
        assert!(content.contains("amp.on('tool.call'"));
        assert!(content.contains("amp.on('tool.result'"));
        assert!(content.contains("filesModifiedByToolCall"));
        assert!(content.contains("spawn("));
        assert!(content.contains("'--hook-input', 'stdin'"));
        assert!(content.contains("rev-parse"));
        assert!(content.contains("__GIT_AI_BINARY_PATH__"));
    }

    #[test]
    fn test_amp_plugin_placeholder_substitution() {
        let binary_path = create_test_binary_path();
        let content = AmpInstaller::generate_plugin_content(&binary_path);

        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(content.contains(r#"const GIT_AI_BIN = '/usr/local/bin/git-ai'"#));
        assert!(content.contains("'checkpoint'"));
        assert!(content.contains("'amp'"));
        assert!(content.contains("'--hook-input'"));
        assert!(content.contains("'stdin'"));
    }

    #[test]
    fn test_amp_plugin_windows_path_escaping() {
        let binary_path = PathBuf::from(r"C:\Users\foo\.git-ai\bin\git-ai.exe");
        let content = AmpInstaller::generate_plugin_content(&binary_path);

        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        assert!(
            content.contains(r#"const GIT_AI_BIN = 'C:\\Users\\foo\\.git-ai\\bin\\git-ai.exe'"#)
        );
    }

    #[test]
    fn test_amp_plugin_handles_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let binary_path = create_test_binary_path();
        let plugin_path = temp_dir
            .path()
            .join(".config")
            .join("amp")
            .join("plugins")
            .join("git-ai.ts");

        assert!(!plugin_path.parent().unwrap().exists());

        if let Some(parent) = plugin_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let generated = AmpInstaller::generate_plugin_content(&binary_path);
        fs::write(&plugin_path, &generated).unwrap();

        assert!(plugin_path.exists());
        let content = fs::read_to_string(&plugin_path).unwrap();
        assert!(content.contains("tool.call"));
        assert!(content.contains("tool.result"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
    }
}
