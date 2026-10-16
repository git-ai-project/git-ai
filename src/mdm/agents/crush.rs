use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{binary_exists, generate_diff, home_dir, write_atomic};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const CRUSH_HOOK_SCRIPT_CONTENT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/crush/git-ai-hook.sh"
));

/// Tools that can modify tracked files. Read-only bash invocations are
/// filtered inside the hook script itself.
const CRUSH_MATCHER: &str = "^(edit|write|multiedit|lsp_replace_symbol|lsp_rename|bash)$";

pub struct CrushInstaller;

impl CrushInstaller {
    fn config_dir() -> PathBuf {
        if let Ok(test_path) = std::env::var("GIT_AI_CRUSH_CONFIG_PATH") {
            return PathBuf::from(test_path);
        }
        home_dir().join(".config").join("crush")
    }

    fn config_path() -> PathBuf {
        Self::config_dir().join("crush.json")
    }

    fn script_path() -> PathBuf {
        Self::config_dir()
            .join("hooks")
            .join("git-ai-checkpoint.sh")
    }

    /// Escape a string for use inside single quotes in POSIX shell source.
    /// Single quotes preserve everything literally except the quote itself,
    /// which is closed, escaped, and reopened (`'\''`).
    fn shell_single_quote_escape(value: &str) -> String {
        value.replace('\'', r"'\''")
    }

    fn generate_script_content(binary_path: &Path) -> String {
        let path_str = Self::shell_single_quote_escape(&binary_path.display().to_string());
        CRUSH_HOOK_SCRIPT_CONTENT.replace("__GIT_AI_BINARY_PATH__", &path_str)
    }

    fn expected_hook_entry(script_path: &Path) -> Value {
        json!({
            "matcher": CRUSH_MATCHER,
            "command": script_path.display().to_string(),
        })
    }

    /// Returns `(hooks_installed, hooks_up_to_date)` for the hook entry in
    /// crush.json. `hooks_installed` = a hook entry referencing our script
    /// exists; `hooks_up_to_date` = the entry matches the expected matcher
    /// and command exactly.
    fn hook_status(config: &Value, script_path: &Path) -> (bool, bool) {
        let blocks = config
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|v| v.as_array());

        let Some(blocks) = blocks else {
            return (false, false);
        };

        let expected = Self::expected_hook_entry(script_path);
        let mut installed = false;
        let mut up_to_date = false;

        for block in blocks {
            let command = block.get("command").and_then(|c| c.as_str()).unwrap_or("");
            // Only exact matches against our installed script path count as
            // ours; substring matching would let us claim (and later remove)
            // unrelated user hooks that merely mention git-ai.
            let is_ours = command == script_path.display().to_string();
            if !is_ours {
                continue;
            }
            installed = true;
            if *block == expected {
                up_to_date = true;
            }
        }

        (installed, up_to_date)
    }

    /// Read and parse crush.json, returning an empty object for missing or
    /// empty files.
    fn read_config(config_path: &Path) -> Result<Value, GitAiError> {
        if !config_path.exists() {
            return Ok(json!({}));
        }
        let content = fs::read_to_string(config_path)?;
        if content.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&content).map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to parse Crush config {}: {}",
                config_path.display(),
                e
            ))
        })
    }

    fn render_config(config: &Value) -> Result<String, GitAiError> {
        serde_json::to_string_pretty(config)
            .map_err(|e| GitAiError::Generic(format!("Failed to serialize Crush config: {e}")))
            .map(|s| s + "\n")
    }

    fn install_script(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let script_path = Self::script_path();
        let existing = fs::read_to_string(&script_path).unwrap_or_default();
        let new_content = Self::generate_script_content(&params.binary_path);

        if existing.trim() == new_content.trim() {
            return Ok(None);
        }

        let diff = generate_diff(&script_path, &existing, &new_content);
        if !dry_run {
            if let Some(dir) = script_path.parent() {
                fs::create_dir_all(dir).map_err(|e| {
                    GitAiError::Generic(format!(
                        "Failed to create Crush hooks directory {}: {}",
                        dir.display(),
                        e
                    ))
                })?;
            }
            write_atomic(&script_path, new_content.as_bytes()).map_err(|e| {
                GitAiError::Generic(format!(
                    "Failed to write Crush hook script {}: {}",
                    script_path.display(),
                    e
                ))
            })?;
            Self::make_executable(&script_path)?;
        }
        Ok(Some(diff))
    }

    fn make_executable(path: &Path) -> Result<(), GitAiError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path)
                .map_err(|e| {
                    GitAiError::Generic(format!(
                        "Failed to read Crush hook script metadata {}: {}",
                        path.display(),
                        e
                    ))
                })?
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).map_err(|e| {
                GitAiError::Generic(format!(
                    "Failed to make Crush hook script executable {}: {}",
                    path.display(),
                    e
                ))
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }

    /// Fail unless the hook script exists and is executable, so the config
    /// entry never points at a script Crush cannot run.
    fn verify_script_executable(script_path: &Path) -> Result<(), GitAiError> {
        if !script_path.is_file() {
            return Err(GitAiError::Generic(format!(
                "Crush hook script not found at {}",
                script_path.display()
            )));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(script_path)
                .map_err(|e| {
                    GitAiError::Generic(format!(
                        "Failed to read Crush hook script metadata {}: {}",
                        script_path.display(),
                        e
                    ))
                })?
                .permissions()
                .mode();
            if mode & 0o111 == 0 {
                return Err(GitAiError::Generic(format!(
                    "Crush hook script is not executable: {}",
                    script_path.display()
                )));
            }
        }

        Ok(())
    }
}

impl HookInstaller for CrushInstaller {
    fn name(&self) -> &str {
        "Crush"
    }

    fn id(&self) -> &str {
        "crush"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("crush");
        let has_global_config = Self::config_dir().exists();
        let has_local_config = Path::new(".crush").exists();

        if !has_binary && !has_global_config && !has_local_config {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let script_path = Self::script_path();
        if !script_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let config = Self::read_config(&Self::config_path())?;
        let (hooks_installed, hooks_up_to_date) = Self::hook_status(&config, &script_path);

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed,
            hooks_up_to_date,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let config_path = Self::config_path();
        let script_path = Self::script_path();

        let mut diffs: Vec<String> = Vec::new();
        if let Some(diff) = self.install_script(params, dry_run)? {
            diffs.push(diff);
        }

        // The config entry points at the script; never register it unless
        // the script is present and executable.
        if !dry_run {
            Self::verify_script_executable(&script_path)?;
        }

        let existing = Self::read_config(&config_path)?;
        let (installed, up_to_date) = Self::hook_status(&existing, &script_path);
        if !installed || !up_to_date {
            let mut merged = existing.clone();
            let hooks_obj = merged
                .as_object_mut()
                .ok_or_else(|| {
                    GitAiError::Generic(format!(
                        "Crush config {} is not a JSON object",
                        config_path.display()
                    ))
                })?
                .entry("hooks")
                .or_insert_with(|| json!({}));

            let hooks_obj = hooks_obj
                .as_object_mut()
                .ok_or_else(|| GitAiError::Generic("Crush config hooks is not an object".into()))?;

            let mut pre_tool_array = hooks_obj
                .get("PreToolUse")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Replace only entries that point at our exact script path
            // (e.g. stale matchers from earlier installs); unrelated user
            // hooks are preserved even if they mention git-ai.
            let our_command = script_path.display().to_string();
            pre_tool_array.retain(|block| {
                let command = block.get("command").and_then(|c| c.as_str()).unwrap_or("");
                command != our_command
            });
            pre_tool_array.push(Self::expected_hook_entry(&script_path));

            hooks_obj.insert(
                "PreToolUse".to_string(),
                Value::Array(pre_tool_array.clone()),
            );

            let existing_content = fs::read_to_string(&config_path).unwrap_or_default();
            let new_content = Self::render_config(&merged)?;

            if existing_content.trim() != new_content.trim() {
                diffs.push(generate_diff(&config_path, &existing_content, &new_content));
                if !dry_run {
                    if let Some(dir) = config_path.parent() {
                        fs::create_dir_all(dir)?;
                    }
                    write_atomic(&config_path, new_content.as_bytes())?;
                }
            }
        }

        if diffs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(diffs.join("\n")))
        }
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        let config_path = Self::config_path();
        let script_path = Self::script_path();

        let mut diffs: Vec<String> = Vec::new();

        if config_path.exists() {
            let existing = Self::read_config(&config_path)?;
            if let Some(hooks_obj) = existing.get("hooks").and_then(|h| h.as_object()) {
                if let Some(pre_tool_array) = hooks_obj.get("PreToolUse").and_then(|v| v.as_array())
                {
                    let our_command = script_path.display().to_string();
                    let filtered: Vec<Value> = pre_tool_array
                        .iter()
                        .filter(|block| {
                            let command =
                                block.get("command").and_then(|c| c.as_str()).unwrap_or("");
                            command != our_command
                        })
                        .cloned()
                        .collect();

                    if filtered.len() != pre_tool_array.len() {
                        let mut merged = existing.clone();
                        if let Some(hooks_mut) =
                            merged.get_mut("hooks").and_then(|h| h.as_object_mut())
                        {
                            if filtered.is_empty() {
                                hooks_mut.remove("PreToolUse");
                                if hooks_mut.is_empty() {
                                    if let Some(obj) = merged.as_object_mut() {
                                        obj.remove("hooks");
                                    }
                                }
                            } else {
                                hooks_mut.insert("PreToolUse".to_string(), Value::Array(filtered));
                            }
                        }

                        let existing_content = fs::read_to_string(&config_path)?;
                        let new_content = Self::render_config(&merged)?;
                        diffs.push(generate_diff(&config_path, &existing_content, &new_content));
                        if !dry_run {
                            write_atomic(&config_path, new_content.as_bytes())?;
                        }
                    }
                }
            }
        }

        if script_path.exists() {
            let existing_content = fs::read_to_string(&script_path)?;
            diffs.push(generate_diff(&script_path, &existing_content, ""));
            if !dry_run {
                fs::remove_file(&script_path)?;
            }
        }

        if diffs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(diffs.join("\n")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    #[test]
    fn test_crush_script_output_path() {
        assert_eq!(
            CrushInstaller::script_path(),
            home_dir()
                .join(".config")
                .join("crush")
                .join("hooks")
                .join("git-ai-checkpoint.sh")
        );
    }

    #[test]
    fn test_crush_script_content_contains_checkpoint_command() {
        let content = CrushInstaller::generate_script_content(&create_test_binary_path());
        assert!(content.contains("GIT_AI_BIN='/usr/local/bin/git-ai'"));
        assert!(content.contains("checkpoint crush --hook-input stdin"));
        assert!(content.contains("PostToolUse"));
        assert!(content.contains("PreToolUse"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
    }

    #[test]
    fn test_crush_script_content_uses_sequential_flush_with_fallback_timer() {
        let content = CrushInstaller::generate_script_content(&create_test_binary_path());
        // Long-running tool calls are attributed by flushing the pending
        // post checkpoint when the next PreToolUse fires (tools run
        // sequentially); the sleep timer is only a last-tool fallback.
        assert!(content.contains("emit_pending_post"));
        assert!(content.contains("counter_file"));
        assert!(content.contains("FALLBACK_DELAY"));
        assert!(content.contains("$invocation"));
    }

    #[test]
    fn test_crush_script_content_aborts_when_pre_checkpoint_fails() {
        let content = CrushInstaller::generate_script_content(&create_test_binary_path());
        // Without a successful human pre-mark baseline, a synthetic post
        // could credit earlier human edits to the AI; the wrapper must
        // check the pre checkpoint exit status before scheduling the post.
        assert!(content.contains("if ! printf '%s' \"$input\""));
        assert!(content.contains("checkpoint crush --hook-input stdin >/dev/null 2>&1; then"));
        // The pending payload must be written only after the guarded
        // pre checkpoint, never before it.
        let guard_pos = content
            .find("checkpoint crush --hook-input stdin >/dev/null 2>&1; then")
            .expect("pre checkpoint guard should exist");
        let pending_pos = content
            .find("printf '%s' \"$input\" >\"$state_dir/pending\"")
            .expect("pending write should exist");
        assert!(
            pending_pos > guard_pos,
            "pending payload must be written after the pre checkpoint guard"
        );
    }

    #[test]
    fn test_crush_script_content_escapes_apostrophes_in_binary_path() {
        let binary_path = PathBuf::from("/opt/doesn't work/git-ai");
        let content = CrushInstaller::generate_script_content(&binary_path);
        // The embedded path must close the quote, escape the apostrophe, and
        // reopen the quote; a raw apostrophe would break out of the shell
        // string and allow command injection.
        assert!(content.contains(r"GIT_AI_BIN='/opt/doesn'\''t work/git-ai'"));
    }

    #[test]
    fn test_crush_hook_status_empty_config() {
        let script_path = PathBuf::from("/home/user/.config/crush/hooks/git-ai-checkpoint.sh");
        let (installed, up_to_date) = CrushInstaller::hook_status(&json!({}), &script_path);
        assert!(!installed);
        assert!(!up_to_date);
    }

    #[test]
    fn test_crush_verify_script_executable_rejects_missing_script() {
        let missing = PathBuf::from("/nonexistent/git-ai-checkpoint.sh");
        assert!(CrushInstaller::verify_script_executable(&missing).is_err());
    }

    #[test]
    fn test_crush_verify_script_executable_rejects_non_executable_script() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("git-ai-checkpoint.sh");
        fs::write(&script, "#!/usr/bin/env bash\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o644);
            fs::set_permissions(&script, permissions).unwrap();
            assert!(CrushInstaller::verify_script_executable(&script).is_err());
        }

        #[cfg(not(unix))]
        {
            assert!(CrushInstaller::verify_script_executable(&script).is_ok());
        }
    }

    #[test]
    fn test_crush_make_executable_errors_on_missing_file() {
        let missing = PathBuf::from("/nonexistent/git-ai-checkpoint.sh");
        assert!(CrushInstaller::make_executable(&missing).is_err());
    }

    #[test]
    fn test_crush_hook_status_with_entry() {
        let script_path = PathBuf::from("/home/user/.config/crush/hooks/git-ai-checkpoint.sh");
        let config = json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "^(edit|write|multiedit|lsp_replace_symbol|lsp_rename|bash)$",
                     "command": "/home/user/.config/crush/hooks/git-ai-checkpoint.sh"}
                ]
            }
        });
        let (installed, up_to_date) = CrushInstaller::hook_status(&config, &script_path);
        assert!(installed);
        assert!(up_to_date);
    }

    #[test]
    fn test_crush_hook_status_stale_matcher_not_up_to_date() {
        let script_path = PathBuf::from("/home/user/.config/crush/hooks/git-ai-checkpoint.sh");
        let config = json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "^edit$", "command": script_path.display().to_string()}
                ]
            }
        });
        let (installed, up_to_date) = CrushInstaller::hook_status(&config, &script_path);
        assert!(installed);
        assert!(!up_to_date);
    }

    #[test]
    fn test_crush_hook_status_preserves_unrelated_entries() {
        let script_path = PathBuf::from("/home/user/.config/crush/hooks/git-ai-checkpoint.sh");
        let config = json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "^bash$", "command": "/some/other/hook.sh"}
                ]
            }
        });
        let (installed, _up_to_date) = CrushInstaller::hook_status(&config, &script_path);
        assert!(!installed);
    }

    #[test]
    fn test_crush_hook_status_does_not_claim_user_hooks_mentioning_git_ai() {
        // A user-owned wrapper that happens to invoke `git-ai checkpoint`
        // must not be treated as ours; otherwise install/uninstall would
        // rewrite or silently delete it.
        let script_path = PathBuf::from("/home/user/.config/crush/hooks/git-ai-checkpoint.sh");
        let config = json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "^bash$",
                     "command": "/home/user/bin/my-wrapper.sh | git-ai checkpoint claude --hook-input stdin"},
                    {"matcher": "^edit$",
                     "command": "/home/user/.config/crush/hooks/../hooks/git-ai-checkpoint.sh"}
                ]
            }
        });
        let (installed, _up_to_date) = CrushInstaller::hook_status(&config, &script_path);
        assert!(
            !installed,
            "only exact script path matches should count as our hook"
        );
    }
}
