use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult,
};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use std::fs;
use std::path::PathBuf;

// Extension source files embedded at compile time
const EXTENSION_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/zed/extension.toml"
));

const EXTENSION_CARGO_TOML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/zed/Cargo.toml"
));

const EXTENSION_LIB_RS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/zed/src/lib.rs"
));

const HOOK_SCRIPT_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/zed/git-ai-zed-hook.sh"
));

pub struct ZedInstaller;

impl ZedInstaller {
    /// Directory where the Zed extension source is installed.
    /// Zed discovers extensions placed here (user-installed extensions).
    fn extension_dir() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            home_dir()
                .join("Library")
                .join("Application Support")
                .join("Zed")
                .join("extensions")
                .join("installed")
                .join("git-ai")
        }
        #[cfg(not(target_os = "macos"))]
        {
            home_dir()
                .join(".config")
                .join("zed")
                .join("extensions")
                .join("installed")
                .join("git-ai")
        }
    }

    /// Path where the hook wrapper script is installed.
    fn hook_script_path() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            home_dir()
                .join("Library")
                .join("Application Support")
                .join("Zed")
                .join("git-ai-zed-hook.sh")
        }
        #[cfg(not(target_os = "macos"))]
        {
            home_dir()
                .join(".config")
                .join("zed")
                .join("git-ai-zed-hook.sh")
        }
    }

    /// Path to Zed's user settings.json.
    fn settings_path() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            home_dir()
                .join("Library")
                .join("Application Support")
                .join("Zed")
                .join("settings.json")
        }
        #[cfg(not(target_os = "macos"))]
        {
            home_dir().join(".config").join("zed").join("settings.json")
        }
    }

    fn is_extension_installed() -> bool {
        Self::extension_dir().join("extension.toml").exists()
    }

    fn is_hook_script_installed(binary_path: &std::path::Path) -> bool {
        let script_path = Self::hook_script_path();
        if !script_path.exists() {
            return false;
        }
        let expected = Self::generate_hook_script(binary_path);
        let current = fs::read_to_string(&script_path).unwrap_or_default();
        current.trim() == expected.trim()
    }

    fn generate_hook_script(binary_path: &std::path::Path) -> String {
        let path_str = binary_path.display().to_string().replace('\\', "\\\\");
        HOOK_SCRIPT_TEMPLATE.replace("__GIT_AI_BINARY_PATH__", &path_str)
    }

    /// Check whether Zed's settings.json already contains the format_on_save
    /// configuration for the git-ai hook script.
    fn is_settings_configured(script_path: &std::path::Path) -> bool {
        let settings = Self::settings_path();
        if !settings.exists() {
            return false;
        }
        let content = fs::read_to_string(&settings).unwrap_or_default();
        content.contains(&script_path.display().to_string())
    }

    /// Write the format_on_save external command into Zed's settings.json.
    ///
    /// Zed's settings.json is JSONC.  We use a simple string-based approach
    /// to inject the formatter block without disturbing any existing content.
    ///
    /// The resulting snippet (at the top level) looks like:
    /// ```jsonc
    /// "formatter": {
    ///   "external": {
    ///     "command": "/path/to/git-ai-zed-hook.sh",
    ///     "arguments": []
    ///   }
    /// },
    /// "format_on_save": "on"
    /// ```
    ///
    /// We only inject when the key is not already present to avoid clobbering
    /// user-configured formatters.
    fn install_settings(
        binary_path: &std::path::Path,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let script_path = Self::hook_script_path();
        let settings_path = Self::settings_path();

        let original = if settings_path.exists() {
            fs::read_to_string(&settings_path)?
        } else {
            String::new()
        };

        // If the script path is already referenced, or if the user already has a
        // "formatter" key, skip injection to avoid clobbering their config or
        // producing invalid JSON with duplicate keys.
        if original.contains(&script_path.display().to_string())
            || original.contains("\"formatter\"")
        {
            return Ok(None);
        }

        let script_str = script_path.display().to_string().replace('\\', "\\\\");

        let formatter_snippet = format!(
            r#"  "formatter": {{
    "external": {{
      "command": "{script_str}",
      "arguments": []
    }}
  }},
  "format_on_save": "on""#
        );

        // Insert the snippet into the existing JSON object or create a new one.
        let new_content = if original.trim().is_empty() || !original.contains('{') {
            format!("{{\n{formatter_snippet}\n}}\n")
        } else {
            // Find the last `}` and insert before it.
            let trimmed = original.trim_end_matches(['\n', '\r']);
            if let Some(pos) = trimmed.rfind('}') {
                let (before, _after) = trimmed.split_at(pos);
                let before = before.trim_end_matches(',');
                format!("{before},\n{formatter_snippet}\n}}\n")
            } else {
                format!("{{\n{formatter_snippet}\n}}\n")
            }
        };

        let diff = generate_diff(&settings_path, &original, &new_content);

        if !dry_run {
            if let Some(parent) = settings_path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_atomic(&settings_path, new_content.as_bytes())?;
        }

        let _ = binary_path; // used indirectly via script path
        Ok(Some(diff))
    }

    fn uninstall_settings(dry_run: bool) -> Result<Option<String>, GitAiError> {
        let script_path = Self::hook_script_path();
        let settings_path = Self::settings_path();

        if !settings_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&settings_path)?;
        if !content.contains(&script_path.display().to_string()) {
            return Ok(None);
        }

        // Remove the formatter lines that reference our script.
        // We do a line-by-line removal of any line containing our script path
        // and the surrounding "formatter" block.
        let new_content = remove_formatter_block(&content, &script_path.display().to_string());

        if new_content.trim() == content.trim() {
            return Ok(None);
        }

        let diff = generate_diff(&settings_path, &content, &new_content);

        if !dry_run {
            write_atomic(&settings_path, new_content.as_bytes())?;
        }

        Ok(Some(diff))
    }
}

/// Remove the formatter block referencing `script_path_str` from settings content.
fn remove_formatter_block(content: &str, script_path_str: &str) -> String {
    // Strategy: remove lines that contain the script path and any immediately
    // adjacent "formatter" / "format_on_save" lines we inserted.
    let mut out = Vec::new();
    let mut skip_count = 0usize;
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    let mut i = 0;
    while i < n {
        if skip_count > 0 {
            skip_count -= 1;
            i += 1;
            continue;
        }
        let line = lines[i];
        if line.contains(script_path_str) {
            // Remove this line plus look back to remove enclosing formatter block.
            // The block we inserted looks like (3-5 lines):
            //   "formatter": {         <- i-2
            //     "external": {        <- i-1
            //       "command": "...",  <- i   (contains script path)
            //       "arguments": []   <- i+1
            //     }                   <- i+2
            //   },                    <- i+3 or merged with format_on_save
            //   "format_on_save": "on" <- next line after block close
            //
            // Pop already-emitted lines back to the "formatter" line.
            // Only remove lines that are part of our injected block: lines
            // containing "external" (our wrapper key), lines that are only
            // whitespace/commas, and the "formatter" line — but ONLY if it
            // contains "\"formatter\"" with no other content that would
            // indicate it is a different key (e.g. "default_formatter").
            while out
                .last()
                .map(|l: &String| {
                    l.contains("\"external\"") || l.trim().is_empty() || l.trim() == "," || {
                        // Remove the "formatter" line only if it matches
                        // exactly our injected pattern and not an unrelated
                        // key like "default_formatter".
                        let trimmed = l.trim().trim_start_matches('"');
                        trimmed.starts_with("formatter\"")
                    }
                })
                .unwrap_or(false)
            {
                out.pop();
            }
            // Skip ahead past closing braces and format_on_save
            let mut j = i + 1;
            while j < n
                && (lines[j].trim() == "],"
                    || lines[j].trim() == "]"
                    || lines[j].trim() == "},"
                    || lines[j].trim() == "}"
                    || lines[j].contains("\"arguments\"")
                    || lines[j].contains("\"format_on_save\""))
            {
                j += 1;
            }
            skip_count = j - i - 1;
        } else {
            out.push(line.to_string());
        }
        i += 1;
    }
    out.join("\n") + "\n"
}

impl HookInstaller for ZedInstaller {
    fn name(&self) -> &str {
        "Zed"
    }

    fn id(&self) -> &str {
        "zed"
    }

    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn process_names(&self) -> Vec<&str> {
        vec!["zed"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let tool_installed =
            binary_exists("zed") || home_dir().join(".config").join("zed").exists() || {
                #[cfg(target_os = "macos")]
                {
                    std::path::Path::new("/Applications/Zed.app").exists()
                        || home_dir().join("Applications").join("Zed.app").exists()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    false
                }
            };

        let script_installed = Self::is_hook_script_installed(&params.binary_path);
        let extension_installed = Self::is_extension_installed();
        let settings_configured = Self::is_settings_configured(&Self::hook_script_path());

        let hooks_installed = script_installed && extension_installed && settings_configured;

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
        // All installation is done in install_extras.
        Ok(None)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        // All uninstallation is done in uninstall_extras.
        Ok(None)
    }

    fn install_extras(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        let mut results = Vec::new();

        // --- 1. Install hook script ---
        let script_path = Self::hook_script_path();
        let script_content = Self::generate_hook_script(&params.binary_path);

        let existing_script = if script_path.exists() {
            fs::read_to_string(&script_path)?
        } else {
            String::new()
        };

        if existing_script.trim() == script_content.trim() {
            results.push(InstallResult {
                changed: false,
                diff: None,
                message: format!(
                    "Zed: hook script already up-to-date at {}",
                    script_path.display()
                ),
            });
        } else {
            let diff = generate_diff(&script_path, &existing_script, &script_content);
            if !dry_run {
                if let Some(parent) = script_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                write_atomic(&script_path, script_content.as_bytes())?;
                // Make executable on Unix
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&script_path)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&script_path, perms)?;
                }
            }
            results.push(InstallResult {
                changed: true,
                diff: Some(diff),
                message: format!("Zed: hook script installed to {}", script_path.display()),
            });
        }

        // --- 2. Install extension source ---
        let ext_dir = Self::extension_dir();
        if Self::is_extension_installed() {
            results.push(InstallResult {
                changed: false,
                diff: None,
                message: "Zed: extension source already installed".to_string(),
            });
        } else if dry_run {
            results.push(InstallResult {
                changed: true,
                diff: None,
                message: format!("Zed: pending extension install to {}", ext_dir.display()),
            });
        } else {
            fs::create_dir_all(&ext_dir)?;
            fs::write(ext_dir.join("extension.toml"), EXTENSION_TOML)?;
            fs::write(ext_dir.join("Cargo.toml"), EXTENSION_CARGO_TOML)?;

            let src_dir = ext_dir.join("src");
            fs::create_dir_all(&src_dir)?;
            fs::write(src_dir.join("lib.rs"), EXTENSION_LIB_RS)?;

            results.push(InstallResult {
                changed: true,
                diff: None,
                message: format!(
                    "Zed: extension source installed to {}. Restart Zed to activate.",
                    ext_dir.display()
                ),
            });
        }

        // --- 3. Configure format_on_save in settings.json ---
        match Self::install_settings(&params.binary_path, dry_run)? {
            Some(diff) => {
                results.push(InstallResult {
                    changed: true,
                    diff: Some(diff),
                    message: format!(
                        "Zed: format_on_save configured in {}",
                        Self::settings_path().display()
                    ),
                });
            }
            None => {
                results.push(InstallResult {
                    changed: false,
                    diff: None,
                    message: format!(
                        "Zed: settings.json already configured at {}",
                        Self::settings_path().display()
                    ),
                });
            }
        }

        Ok(results)
    }

    fn uninstall_extras(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<UninstallResult>, GitAiError> {
        let mut results = Vec::new();

        // --- 1. Remove hook script ---
        let script_path = Self::hook_script_path();
        if script_path.exists() {
            let content = fs::read_to_string(&script_path)?;
            let diff = generate_diff(&script_path, &content, "");
            if !dry_run {
                fs::remove_file(&script_path)?;
            }
            results.push(UninstallResult {
                changed: true,
                diff: Some(diff),
                message: format!("Zed: hook script removed from {}", script_path.display()),
            });
        } else {
            results.push(UninstallResult {
                changed: false,
                diff: None,
                message: "Zed: hook script was not installed".to_string(),
            });
        }

        // --- 2. Remove extension source ---
        let ext_dir = Self::extension_dir();
        if ext_dir.exists() {
            if !dry_run {
                fs::remove_dir_all(&ext_dir)?;
            }
            results.push(UninstallResult {
                changed: true,
                diff: None,
                message: format!("Zed: extension removed from {}", ext_dir.display()),
            });
        } else {
            results.push(UninstallResult {
                changed: false,
                diff: None,
                message: "Zed: extension was not installed".to_string(),
            });
        }

        // --- 3. Remove format_on_save configuration ---
        match Self::uninstall_settings(dry_run)? {
            Some(diff) => {
                results.push(UninstallResult {
                    changed: true,
                    diff: Some(diff),
                    message: format!(
                        "Zed: format_on_save removed from {}",
                        Self::settings_path().display()
                    ),
                });
            }
            None => {
                results.push(UninstallResult {
                    changed: false,
                    diff: None,
                    message: "Zed: settings.json had no git-ai formatter configured".to_string(),
                });
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_zed_installer_name() {
        assert_eq!(ZedInstaller.name(), "Zed");
    }

    #[test]
    fn test_zed_installer_id() {
        assert_eq!(ZedInstaller.id(), "zed");
    }

    #[test]
    fn test_generate_hook_script_substitutes_binary_path() {
        let binary = Path::new("/usr/local/bin/git-ai");
        let script = ZedInstaller::generate_hook_script(binary);
        assert!(!script.contains("__GIT_AI_BINARY_PATH__"));
        assert!(script.contains("/usr/local/bin/git-ai"));
    }

    #[test]
    fn test_generate_hook_script_escapes_windows_backslashes() {
        let binary = std::path::PathBuf::from(r"C:\Users\test\.git-ai\bin\git-ai.exe");
        let script = ZedInstaller::generate_hook_script(&binary);
        assert!(!script.contains("__GIT_AI_BINARY_PATH__"));
        // Backslashes should be doubled for the shell string
        assert!(script.contains(r"C:\\Users\\test\\.git-ai\\bin\\git-ai.exe"));
    }

    #[test]
    fn test_remove_formatter_block_cleans_inserted_snippet() {
        let script_path = "/home/user/.config/zed/git-ai-zed-hook.sh";
        let content = r#"{
  "other_setting": true,
  "formatter": {
    "external": {
      "command": "/home/user/.config/zed/git-ai-zed-hook.sh",
      "arguments": []
    }
  },
  "format_on_save": "on"
}
"#;
        let result = remove_formatter_block(content, script_path);
        assert!(!result.contains("git-ai-zed-hook.sh"));
        assert!(result.contains("other_setting"));
    }

    #[test]
    fn test_hook_script_passthrough_contract() {
        // Verify the script template contains the pass-through stdout write
        assert!(HOOK_SCRIPT_TEMPLATE.contains("printf '%s' \"$CONTENT\""));
    }

    #[test]
    fn test_hook_script_contains_debounce() {
        assert!(HOOK_SCRIPT_TEMPLATE.contains("sleep 0.5"));
        assert!(HOOK_SCRIPT_TEMPLATE.contains("MY_TOKEN"));
    }

    #[test]
    fn test_hook_script_fires_checkpoint() {
        assert!(HOOK_SCRIPT_TEMPLATE.contains("checkpoint known_human"));
        assert!(HOOK_SCRIPT_TEMPLATE.contains("--hook-input stdin"));
    }

    #[test]
    fn test_extension_toml_has_correct_id() {
        assert!(EXTENSION_TOML.contains("id = \"git-ai\""));
    }

    #[test]
    fn test_extension_lib_rs_has_register_macro() {
        assert!(EXTENSION_LIB_RS.contains("register_extension!"));
    }
}
