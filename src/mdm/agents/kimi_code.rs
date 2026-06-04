//! Hook installer for Kimi Code (Moonshot AI / kimi-cli).
//!
//! Kimi Code is a Python-based CLI from Moonshot AI. Hooks are configured in
//! `~/.kimi/config.toml` using TOML array-of-tables (`[[hooks]]`) with the
//! schema defined in the upstream docs at
//! `https://moonshotai.github.io/kimi-cli/en/customization/hooks.html`:
//!
//! ```toml
//! [[hooks]]
//! event = "PostToolUse"
//! command = "..."
//! matcher = "WriteFile|StrReplaceFile"   # optional regex
//! timeout = 30                            # optional, default 30s
//! ```
//!
//! We install one entry for `PreToolUse` and one for `PostToolUse`, both
//! running `git-ai checkpoint kimi-code --hook-input stdin`. We omit the
//! `matcher` field deliberately so kimi-cli routes ALL tool events to us;
//! the preset itself is responsible for filtering events to the tools we
//! actually checkpoint (`WriteFile`, `StrReplaceFile`, `Shell`).
//!
//! The installer is idempotent: re-running it leaves the config unchanged
//! when our entries are already present and current. Only entries we own
//! (matched via `is_git_ai_kimi_command`) are touched on uninstall, so
//! user-defined hooks survive.

use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic,
};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;
use toml::map::Map;

const KIMI_CHECKPOINT_CMD: &str = "checkpoint kimi-code --hook-input stdin";
const KIMI_HOOK_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];

/// Returns true only for git-ai hooks belonging to *this* preset
/// (`git-ai checkpoint kimi-code ...`). The shared
/// `is_git_ai_checkpoint_command` helper matches any
/// `git-ai checkpoint <preset>` line; we further verify the preset name
/// is exactly `kimi-code` (not just a substring like `kimi-code-pro`)
/// by inspecting whitespace-separated tokens.
fn is_git_ai_kimi_command(cmd: &str) -> bool {
    if !is_git_ai_checkpoint_command(cmd) {
        return false;
    }
    // Find the token immediately after `checkpoint`. Use whitespace tokens
    // to avoid the substring trap (e.g. `kimi-code-pro` would otherwise
    // match a `kimi-code` substring check, causing this installer to
    // clobber a hypothetical sibling preset's hooks).
    let mut tokens = cmd.split_whitespace();
    while let Some(t) = tokens.next() {
        if t == "checkpoint"
            && let Some(name) = tokens.next()
        {
            return name == "kimi-code";
        }
    }
    false
}

pub struct KimiCodeInstaller;

impl KimiCodeInstaller {
    fn config_dir() -> PathBuf {
        home_dir().join(".kimi")
    }

    fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    fn desired_command(binary_path: &Path) -> String {
        format!("{} {}", binary_path.display(), KIMI_CHECKPOINT_CMD)
    }

    /// Returns `(hooks_installed, hooks_up_to_date)` from a parsed config.
    /// `hooks_installed` = a git-ai entry exists for at least one event.
    /// `hooks_up_to_date` = an entry exists for *every* event we install.
    fn hook_status(config: &Value, desired_cmd: &str) -> (bool, bool) {
        let Some(hooks) = config.get("hooks").and_then(|h| h.as_array()) else {
            return (false, false);
        };

        let mut found_any = false;
        let mut up_to_date_events: Vec<&str> = Vec::new();

        for hook in hooks {
            let event = hook.get("event").and_then(|v| v.as_str()).unwrap_or("");
            let cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if !is_git_ai_kimi_command(cmd) {
                continue;
            }
            found_any = true;
            if cmd == desired_cmd
                && KIMI_HOOK_EVENTS.contains(&event)
                && !up_to_date_events.contains(&event)
            {
                up_to_date_events.push(event);
            }
        }

        let hooks_up_to_date = KIMI_HOOK_EVENTS
            .iter()
            .all(|e| up_to_date_events.contains(e));
        (found_any, hooks_up_to_date)
    }

    fn install_hooks_at(
        config_path: &Path,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if let Some(dir) = config_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if config_path.exists() {
            fs::read_to_string(config_path)?
        } else {
            String::new()
        };

        let existing_parsed: Value = if existing_content.trim().is_empty() {
            Value::Table(Map::new())
        } else {
            toml::from_str(&existing_content).map_err(|e| {
                GitAiError::Generic(format!("Failed to parse Kimi Code config.toml: {e}"))
            })?
        };

        let mut parsed = existing_parsed.clone();

        let root = parsed.as_table_mut().ok_or_else(|| {
            GitAiError::Generic("Kimi Code config.toml root must be a table".to_string())
        })?;

        // Get a clone of the existing hooks array (or empty), then build the
        // updated array into a fresh Vec. This avoids holding a mutable
        // borrow on `root` while we mutate child entries.
        let mut hooks_arr: Vec<Value> = root
            .get("hooks")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        // Validate that any pre-existing `hooks` entry is an array (not a
        // table named `hooks`, which would be a different TOML construct).
        if let Some(existing_hooks) = root.get("hooks")
            && !existing_hooks.is_array()
        {
            return Err(GitAiError::Generic(
                "Kimi Code config.toml `hooks` field is not an array".to_string(),
            ));
        }

        let desired_cmd = Self::desired_command(&params.binary_path);

        for event in &KIMI_HOOK_EVENTS {
            // Find the FIRST git-ai-owned entry for this event (if any) and
            // update it; remove duplicates of our own command for the same
            // event so we leave exactly one entry per event.
            let mut found_idx: Option<usize> = None;
            for (idx, hook) in hooks_arr.iter().enumerate() {
                let hook_event = hook.get("event").and_then(|v| v.as_str()).unwrap_or("");
                let hook_cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if hook_event == *event && is_git_ai_kimi_command(hook_cmd) {
                    found_idx = Some(idx);
                    break;
                }
            }

            match found_idx {
                Some(idx) => {
                    // Update command in place if it changed.
                    let current_cmd = hooks_arr[idx]
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if current_cmd != desired_cmd
                        && let Some(table) = hooks_arr[idx].as_table_mut()
                    {
                        table.insert("command".to_string(), Value::String(desired_cmd.clone()));
                    }

                    // Drop any duplicate git-ai entries for this event past
                    // the first one we kept.
                    let keep_idx = idx;
                    let mut current_idx = 0usize;
                    hooks_arr.retain(|hook| {
                        let hook_event = hook.get("event").and_then(|v| v.as_str()).unwrap_or("");
                        let hook_cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
                        let result = if current_idx == keep_idx {
                            true
                        } else {
                            !(hook_event == *event && is_git_ai_kimi_command(hook_cmd))
                        };
                        current_idx += 1;
                        result
                    });
                }
                None => {
                    let mut entry = Map::new();
                    entry.insert("event".to_string(), Value::String((*event).to_string()));
                    entry.insert("command".to_string(), Value::String(desired_cmd.clone()));
                    hooks_arr.push(Value::Table(entry));
                }
            }
        }

        root.insert("hooks".to_string(), Value::Array(hooks_arr));

        // Compare structurally: TOML round-trip can reformat whitespace, so
        // textual equality understates idempotency. Only emit a diff when
        // the parsed config actually changed.
        if existing_parsed == parsed {
            return Ok(None);
        }

        let new_content = toml::to_string_pretty(&parsed).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Kimi Code config.toml: {e}"))
        })?;

        let diff_output = generate_diff(config_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(config_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks_at(config_path: &Path, dry_run: bool) -> Result<Option<String>, GitAiError> {
        if !config_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(config_path)?;
        let existing_parsed: Value = match toml::from_str(&existing_content) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };

        let mut parsed = existing_parsed.clone();

        let Some(root) = parsed.as_table_mut() else {
            return Ok(None);
        };

        let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
            return Ok(None);
        };

        let original_len = hooks.len();
        hooks.retain(|hook| {
            let cmd = hook.get("command").and_then(|v| v.as_str()).unwrap_or("");
            !is_git_ai_kimi_command(cmd)
        });

        if hooks.len() == original_len {
            return Ok(None);
        }

        // If hooks array is now empty, remove the key entirely so the file
        // doesn't carry an empty `hooks = []`.
        if hooks.is_empty() {
            root.remove("hooks");
        }

        if existing_parsed == parsed {
            return Ok(None);
        }

        let new_content = toml::to_string_pretty(&parsed).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Kimi Code config.toml: {e}"))
        })?;

        let diff_output = generate_diff(config_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(config_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}

impl HookInstaller for KimiCodeInstaller {
    fn name(&self) -> &str {
        "Kimi Code"
    }

    fn id(&self) -> &str {
        "kimi-code"
    }

    fn process_names(&self) -> Vec<&str> {
        // kimi-cli installs BOTH `kimi` and `kimi-cli` as entry points
        // (verified against upstream `pyproject.toml`'s `[project.scripts]`).
        vec!["kimi", "kimi-cli"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("kimi") || binary_exists("kimi-cli");
        let has_dotfiles = Self::config_dir().exists();

        if !has_binary && !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let config_path = Self::config_path();
        if !config_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(&config_path)?;
        let parsed: Value = match toml::from_str(&content) {
            Ok(v) => v,
            Err(_) => Value::Table(Map::new()),
        };
        let desired_cmd = Self::desired_command(&params.binary_path);
        let (hooks_installed, hooks_up_to_date) = Self::hook_status(&parsed, &desired_cmd);

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
        Self::install_hooks_at(&Self::config_path(), params, dry_run)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Self::uninstall_hooks_at(&Self::config_path(), dry_run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join(".kimi").join("config.toml");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        (temp_dir, config_path)
    }

    fn binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    fn params() -> HookInstallerParams {
        HookInstallerParams {
            binary_path: binary_path(),
        }
    }

    fn expected_cmd() -> String {
        format!("{} {}", binary_path().display(), KIMI_CHECKPOINT_CMD)
    }

    fn read_hooks(path: &Path) -> Vec<Value> {
        let content = fs::read_to_string(path).unwrap();
        let parsed: Value = toml::from_str(&content).unwrap();
        parsed
            .get("hooks")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default()
    }

    fn find_kimi_entries(hooks: &[Value]) -> Vec<Value> {
        hooks
            .iter()
            .filter(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_git_ai_kimi_command)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    // ---- is_git_ai_kimi_command ----

    #[test]
    fn test_is_git_ai_kimi_command_matches() {
        assert!(is_git_ai_kimi_command(
            "/usr/local/bin/git-ai checkpoint kimi-code --hook-input stdin"
        ));
        assert!(is_git_ai_kimi_command(
            "git-ai checkpoint kimi-code --hook-input stdin"
        ));
    }

    #[test]
    fn test_is_git_ai_kimi_command_does_not_match_siblings() {
        // Must not match other git-ai presets — otherwise we'd clobber them.
        assert!(!is_git_ai_kimi_command(
            "git-ai checkpoint claude --hook-input stdin"
        ));
        assert!(!is_git_ai_kimi_command(
            "git-ai checkpoint gemini --hook-input stdin"
        ));
        assert!(!is_git_ai_kimi_command(
            "git-ai checkpoint codex --hook-input stdin"
        ));
        // Non-git-ai commands are also not ours.
        assert!(!is_git_ai_kimi_command("echo unrelated"));
        assert!(!is_git_ai_kimi_command("prettier --write"));
    }

    #[test]
    fn test_is_git_ai_kimi_command_does_not_match_substring_lookalikes() {
        // Token-precise match: a hypothetical sibling preset whose name
        // begins with `kimi-code` (e.g. `kimi-code-pro`) must NOT be
        // misidentified as ours.
        assert!(!is_git_ai_kimi_command(
            "git-ai checkpoint kimi-code-pro --hook-input stdin"
        ));
        assert!(!is_git_ai_kimi_command(
            "git-ai checkpoint kimi-code2 --hook-input stdin"
        ));
        // A wrapper string that mentions our preset name in a comment or
        // env var should not match either, since `checkpoint` does not
        // immediately precede the literal `kimi-code` token.
        assert!(!is_git_ai_kimi_command(
            "echo 'git-ai checkpoint kimi-code-old' && git-ai checkpoint claude"
        ));
    }

    // ---- Install scenarios ----

    #[test]
    fn s1_fresh_install_creates_pre_and_post() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = KimiCodeInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "fresh install should produce a diff");

        let hooks = read_hooks(&path);
        let kimi_entries = find_kimi_entries(&hooks);
        assert_eq!(kimi_entries.len(), 2);

        let events: Vec<&str> = kimi_entries
            .iter()
            .map(|h| h.get("event").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert!(events.contains(&"PreToolUse"));
        assert!(events.contains(&"PostToolUse"));

        for entry in &kimi_entries {
            assert_eq!(
                entry.get("command").and_then(|c| c.as_str()).unwrap(),
                expected_cmd()
            );
        }
    }

    #[test]
    fn s2_idempotent_already_installed() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let initial = format!(
            r#"[[hooks]]
event = "PreToolUse"
command = "{cmd}"

[[hooks]]
event = "PostToolUse"
command = "{cmd}"
"#
        );
        fs::write(&path, initial).unwrap();

        let diff = KimiCodeInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_none(), "idempotent install should return None");
    }

    #[test]
    fn s3_preserves_unrelated_hooks() {
        let (_td, path) = setup_test_env();
        let unrelated = r#"[[hooks]]
event = "PostToolUse"
command = "echo unrelated"
matcher = "WriteFile"

model = "moonshot-v1-128k"

[settings]
temperature = 0.7
"#;
        fs::write(&path, unrelated).unwrap();

        KimiCodeInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // Original entries preserved.
        assert!(content.contains("echo unrelated"), "{content}");
        assert!(content.contains("moonshot-v1-128k"), "{content}");
        assert!(content.contains("temperature"), "{content}");
        // Our entries added.
        let kimi_entries = find_kimi_entries(&read_hooks(&path));
        assert_eq!(kimi_entries.len(), 2);
    }

    #[test]
    fn s4_updates_outdated_command_path() {
        let (_td, path) = setup_test_env();
        let stale = r#"[[hooks]]
event = "PreToolUse"
command = "/old/path/to/git-ai checkpoint kimi-code --hook-input stdin"

[[hooks]]
event = "PostToolUse"
command = "/old/path/to/git-ai checkpoint kimi-code --hook-input stdin"
"#;
        fs::write(&path, stale).unwrap();

        let diff = KimiCodeInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "stale path should produce a diff");

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("/usr/local/bin/git-ai"));
        assert!(!content.contains("/old/path/to/git-ai"));

        let kimi_entries = find_kimi_entries(&read_hooks(&path));
        assert_eq!(kimi_entries.len(), 2, "still exactly two");
    }

    #[test]
    fn s5_dedups_existing_kimi_entries_for_same_event() {
        // If a previous bug or manual edit created two PreToolUse entries
        // for our command, install should collapse them to exactly one.
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let dup = format!(
            r#"[[hooks]]
event = "PreToolUse"
command = "{cmd}"

[[hooks]]
event = "PreToolUse"
command = "{cmd}"

[[hooks]]
event = "PostToolUse"
command = "{cmd}"
"#
        );
        fs::write(&path, dup).unwrap();

        KimiCodeInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let kimi_entries = find_kimi_entries(&read_hooks(&path));
        assert_eq!(kimi_entries.len(), 2, "duplicates should be removed");
    }

    #[test]
    fn s5b_dedups_when_duplicates_have_different_paths() {
        // Two git-ai entries for the same event with DIFFERENT command
        // paths (e.g. one stale, one already updated). Install should keep
        // exactly one entry per event with the desired command.
        let (_td, path) = setup_test_env();
        let dup = r#"[[hooks]]
event = "PreToolUse"
command = "/old/path/git-ai checkpoint kimi-code --hook-input stdin"

[[hooks]]
event = "PreToolUse"
command = "/usr/local/bin/git-ai checkpoint kimi-code --hook-input stdin"
"#;
        fs::write(&path, dup).unwrap();

        KimiCodeInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let hooks = read_hooks(&path);
        let kimi_entries = find_kimi_entries(&hooks);
        // Exactly one PreToolUse + one PostToolUse (newly added).
        assert_eq!(kimi_entries.len(), 2, "{:?}", kimi_entries);
        let pre_entries: Vec<_> = kimi_entries
            .iter()
            .filter(|h| h.get("event").and_then(|v| v.as_str()) == Some("PreToolUse"))
            .collect();
        assert_eq!(pre_entries.len(), 1, "exactly one PreToolUse entry");
        assert_eq!(
            pre_entries[0]
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap(),
            expected_cmd(),
            "remaining entry should carry the desired command"
        );
        // No stale path lingering anywhere.
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("/old/path/git-ai"), "{content}");
    }

    #[test]
    fn s6_no_matcher_field_set() {
        // We deliberately omit the `matcher` field so the preset receives
        // ALL tool events and can filter inside Rust.
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        KimiCodeInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let kimi_entries = find_kimi_entries(&read_hooks(&path));
        for entry in &kimi_entries {
            assert!(
                entry.get("matcher").is_none(),
                "matcher should be omitted; got {:?}",
                entry.get("matcher")
            );
        }
    }

    #[test]
    fn s7_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = KimiCodeInstaller::install_hooks_at(&path, &params(), true).unwrap();
        assert!(diff.is_some(), "dry run still computes a diff");
        assert!(!path.exists(), "dry run must not write the config file");
    }

    #[test]
    fn s8_create_dir_on_first_install() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir
            .path()
            .join("custom")
            .join(".kimi")
            .join("config.toml");
        // Parent does not exist yet.
        assert!(!nested_path.parent().unwrap().exists());

        KimiCodeInstaller::install_hooks_at(&nested_path, &params(), false).unwrap();

        assert!(nested_path.exists());
    }

    // ---- Uninstall scenarios ----

    #[test]
    fn u1_uninstall_removes_only_kimi_entries() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let mixed = format!(
            r#"[[hooks]]
event = "PreToolUse"
command = "{cmd}"

[[hooks]]
event = "PostToolUse"
command = "echo not ours"

[[hooks]]
event = "PostToolUse"
command = "{cmd}"
"#
        );
        fs::write(&path, mixed).unwrap();

        let diff = KimiCodeInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_some());

        let hooks = read_hooks(&path);
        assert_eq!(hooks.len(), 1);
        assert_eq!(
            hooks[0].get("command").and_then(|c| c.as_str()).unwrap(),
            "echo not ours"
        );
    }

    #[test]
    fn u2_uninstall_returns_none_when_no_kimi_entries() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            r#"[[hooks]]
event = "PostToolUse"
command = "echo unrelated"
"#,
        )
        .unwrap();

        let diff = KimiCodeInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u3_uninstall_returns_none_when_config_missing() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = KimiCodeInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u4_uninstall_removes_empty_hooks_key() {
        // After removing all our entries (when there were no others), the
        // `hooks` key itself should be removed so the file isn't left with
        // a dangling `hooks = []`.
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let only_ours = format!(
            r#"[[hooks]]
event = "PreToolUse"
command = "{cmd}"

[[hooks]]
event = "PostToolUse"
command = "{cmd}"
"#
        );
        fs::write(&path, only_ours).unwrap();

        KimiCodeInstaller::uninstall_hooks_at(&path, false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("hooks"),
            "empty hooks key should be removed; got: {content}"
        );
    }

    #[test]
    fn u5_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let initial = format!(
            r#"[[hooks]]
event = "PreToolUse"
command = "{cmd}"
"#
        );
        fs::write(&path, &initial).unwrap();

        let diff = KimiCodeInstaller::uninstall_hooks_at(&path, true).unwrap();
        assert!(diff.is_some());
        // File contents unchanged.
        assert_eq!(fs::read_to_string(&path).unwrap(), initial);
    }

    // ---- Error handling ----

    #[test]
    fn e1_invalid_toml_install_errors() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "this is = not = valid = toml === wrong").unwrap();

        let result = KimiCodeInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to parse Kimi Code config.toml"),
            "{msg}"
        );
    }

    #[test]
    fn e2_invalid_toml_uninstall_returns_none() {
        // Uninstall on a corrupt file should be a no-op rather than an error,
        // matching codex/gemini installers' tolerance.
        let (_td, path) = setup_test_env();
        fs::write(&path, "not = ! valid").unwrap();

        let diff = KimiCodeInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn e3_hooks_field_wrong_type_install_errors() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            r#"[hooks]
not_an_array = true
"#,
        )
        .unwrap();

        let result = KimiCodeInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
    }
}
