//! Hook installer for Augment Code (Auggie CLI).
//!
//! Augment is a Node-based CLI from the Augment team. Hooks are configured
//! in `~/.augment/settings.json` per the upstream docs at
//! `https://docs.augmentcode.com/cli/config` and `https://docs.augmentcode.com/cli/hooks`.
//! The schema mirrors Claude Code's:
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       { "matcher": ".*",
//!         "hooks": [ { "type": "command",
//!                      "command": "/path/to/git-ai checkpoint augment --hook-input stdin" } ] } ] } }
//! ```
//!
//! We install one entry under the `".*"` catch-all matcher for each of
//! `PreToolUse` and `PostToolUse`. The `matcher` field accepts a regex
//! per Augment's docs, with `".*"` as the documented "match everything"
//! default. The preset itself filters tool events to the tools we
//! actually checkpoint (`save-file`, `str-replace-editor`,
//! `remove-files`, `launch-process`).
//!
//! The installer is idempotent: re-running it leaves the config
//! unchanged when our entries are already present and current. Only
//! entries we own (matched via `is_git_ai_augment_command`) are touched
//! on uninstall, so user-defined hooks survive.

use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const AUGMENT_CHECKPOINT_CMD: &str = "checkpoint augment --hook-input stdin";
const AUGMENT_HOOK_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];
const AUGMENT_CATCH_ALL_MATCHER: &str = ".*";

/// Returns true only for git-ai hooks belonging to *this* preset
/// (`git-ai checkpoint augment ...`). The shared
/// `is_git_ai_checkpoint_command` helper matches any
/// `git-ai checkpoint <preset>` line; we further verify the preset name
/// is exactly `augment` (not a substring) by inspecting whitespace
/// tokens.
fn is_git_ai_augment_command(cmd: &str) -> bool {
    if !is_git_ai_checkpoint_command(cmd) {
        return false;
    }
    let mut tokens = cmd.split_whitespace();
    while let Some(t) = tokens.next() {
        if t == "checkpoint"
            && let Some(name) = tokens.next()
        {
            return name == "augment";
        }
    }
    false
}

pub struct AugmentInstaller;

impl AugmentInstaller {
    fn config_dir() -> PathBuf {
        home_dir().join(".augment")
    }

    fn settings_path() -> PathBuf {
        Self::config_dir().join("settings.json")
    }

    fn desired_command(binary_path: &Path) -> String {
        format!("{} {}", binary_path.display(), AUGMENT_CHECKPOINT_CMD)
    }

    /// Returns `(hooks_installed, hooks_up_to_date)`.
    /// `hooks_installed` = a git-ai-augment entry exists for at least one event.
    /// `hooks_up_to_date` = an entry exists for every event we install,
    ///                     in the catch-all matcher block.
    fn hook_status(settings: &Value, desired_cmd: &str) -> (bool, bool) {
        let Some(hooks_obj) = settings.get("hooks").and_then(|h| h.as_object()) else {
            return (false, false);
        };

        let mut hooks_installed = false;
        let mut up_to_date_events: Vec<&str> = Vec::new();

        for event in &AUGMENT_HOOK_EVENTS {
            let Some(blocks) = hooks_obj.get(*event).and_then(|v| v.as_array()) else {
                continue;
            };
            for block in blocks {
                let is_catch_all = block
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(|m| m == AUGMENT_CATCH_ALL_MATCHER)
                    .unwrap_or(false);

                let Some(inner) = block.get("hooks").and_then(|h| h.as_array()) else {
                    continue;
                };
                for hook in inner {
                    let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) else {
                        continue;
                    };
                    if !is_git_ai_augment_command(cmd) {
                        continue;
                    }
                    hooks_installed = true;
                    if is_catch_all && cmd == desired_cmd && !up_to_date_events.contains(event) {
                        up_to_date_events.push(event);
                    }
                }
            }
        }

        let hooks_up_to_date = AUGMENT_HOOK_EVENTS
            .iter()
            .all(|e| up_to_date_events.contains(e));
        (hooks_installed, hooks_up_to_date)
    }

    fn install_hooks_at(
        settings_path: &Path,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if let Some(dir) = settings_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if settings_path.exists() {
            fs::read_to_string(settings_path)?
        } else {
            String::new()
        };

        let existing: Value = if existing_content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing_content).map_err(|e| {
                GitAiError::Generic(format!("Failed to parse Augment settings.json: {e}"))
            })?
        };

        if !existing.is_object() {
            return Err(GitAiError::Generic(
                "Augment settings.json root must be a JSON object".to_string(),
            ));
        }

        let desired_cmd = Self::desired_command(&params.binary_path);

        let mut merged = existing.clone();
        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));
        if !hooks_obj.is_object() {
            return Err(GitAiError::Generic(
                "Augment settings.json `hooks` field must be a JSON object".to_string(),
            ));
        }

        for event in &AUGMENT_HOOK_EVENTS {
            let mut event_array = hooks_obj
                .get(*event)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Step 1: Strip git-ai entries from non-catch-all matcher blocks
            // (migration / cleanup of stale installs). Track empties.
            let mut emptied_by_migration = vec![false; event_array.len()];
            for (i, block) in event_array.iter_mut().enumerate() {
                let is_catch_all = block
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(|m| m == AUGMENT_CATCH_ALL_MATCHER)
                    .unwrap_or(false);
                if !is_catch_all
                    && let Some(hooks) = block.get_mut("hooks").and_then(|h| h.as_array_mut())
                {
                    let before = hooks.len();
                    hooks.retain(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .map(|cmd| !is_git_ai_augment_command(cmd))
                            .unwrap_or(true)
                    });
                    if hooks.is_empty() && before > 0 {
                        emptied_by_migration[i] = true;
                    }
                }
            }
            let mut i = 0;
            event_array.retain(|_| {
                let drop = emptied_by_migration[i];
                i += 1;
                !drop
            });

            // Step 2: Find or create the catch-all matcher block.
            let catch_all_idx = event_array
                .iter()
                .position(|b| {
                    b.get("matcher")
                        .and_then(|m| m.as_str())
                        .map(|m| m == AUGMENT_CATCH_ALL_MATCHER)
                        .unwrap_or(false)
                })
                .unwrap_or_else(|| {
                    event_array.push(json!({
                        "matcher": AUGMENT_CATCH_ALL_MATCHER,
                        "hooks": []
                    }));
                    event_array.len() - 1
                });

            // Step 3: Ensure exactly one git-ai-augment command in the
            // catch-all block.
            let mut hooks_array = event_array[catch_all_idx]
                .get("hooks")
                .and_then(|h| h.as_array())
                .cloned()
                .unwrap_or_default();

            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;
            for (idx, hook) in hooks_array.iter().enumerate() {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str())
                    && is_git_ai_augment_command(cmd)
                    && found_idx.is_none()
                {
                    found_idx = Some(idx);
                    if cmd != desired_cmd {
                        needs_update = true;
                    }
                }
            }

            match found_idx {
                Some(idx) => {
                    if needs_update {
                        hooks_array[idx] = json!({
                            "type": "command",
                            "command": desired_cmd,
                        });
                    }
                    let keep_idx = idx;
                    let mut current = 0;
                    hooks_array.retain(|hook| {
                        if current == keep_idx {
                            current += 1;
                            true
                        } else if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                            let dup = is_git_ai_augment_command(cmd);
                            current += 1;
                            !dup
                        } else {
                            current += 1;
                            true
                        }
                    });
                }
                None => {
                    hooks_array.push(json!({
                        "type": "command",
                        "command": desired_cmd,
                    }));
                }
            }

            if let Some(matcher_block) = event_array[catch_all_idx].as_object_mut() {
                matcher_block.insert("hooks".to_string(), Value::Array(hooks_array));
            }

            if let Some(obj) = hooks_obj.as_object_mut() {
                obj.insert(event.to_string(), Value::Array(event_array));
            }
        }

        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_obj);
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(settings_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(settings_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks_at(
        settings_path: &Path,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if !settings_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(settings_path)?;
        let existing: Value = match serde_json::from_str(&existing_content) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };

        let mut merged = existing.clone();
        let mut hooks_obj = match merged.get("hooks").cloned() {
            Some(h) if h.is_object() => h,
            _ => return Ok(None),
        };

        let mut changed = false;

        for event in &AUGMENT_HOOK_EVENTS {
            if let Some(event_array) = hooks_obj.get_mut(*event).and_then(|v| v.as_array_mut()) {
                for matcher_block in event_array.iter_mut() {
                    if let Some(hooks_array) = matcher_block
                        .get_mut("hooks")
                        .and_then(|h| h.as_array_mut())
                    {
                        let original = hooks_array.len();
                        hooks_array.retain(|hook| {
                            hook.get("command")
                                .and_then(|c| c.as_str())
                                .map(|cmd| !is_git_ai_augment_command(cmd))
                                .unwrap_or(true)
                        });
                        if hooks_array.len() != original {
                            changed = true;
                        }
                    }
                }
            }
        }

        if !changed {
            return Ok(None);
        }

        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_obj);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(settings_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(settings_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}

impl HookInstaller for AugmentInstaller {
    fn name(&self) -> &str {
        "Augment Code"
    }

    fn id(&self) -> &str {
        "augment"
    }

    fn process_names(&self) -> Vec<&str> {
        vec!["auggie"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("auggie");
        let has_dotfiles = Self::config_dir().exists();

        if !has_binary && !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let settings_path = Self::settings_path();
        if !settings_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(&settings_path)?;
        let existing: Value = serde_json::from_str(&content).unwrap_or_else(|_| json!({}));
        let desired_cmd = Self::desired_command(&params.binary_path);
        let (hooks_installed, hooks_up_to_date) = Self::hook_status(&existing, &desired_cmd);

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
        Self::install_hooks_at(&Self::settings_path(), params, dry_run)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Self::uninstall_hooks_at(&Self::settings_path(), dry_run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, PathBuf) {
        let td = TempDir::new().unwrap();
        let path = td.path().join(".augment").join("settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        (td, path)
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
        format!("{} {}", binary_path().display(), AUGMENT_CHECKPOINT_CMD)
    }

    fn read_event_blocks(path: &Path, event: &str) -> Vec<Value> {
        let content = fs::read_to_string(path).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        parsed
            .get("hooks")
            .and_then(|h| h.get(event))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    fn count_git_ai_entries(blocks: &[Value]) -> usize {
        blocks
            .iter()
            .flat_map(|b| {
                b.get("hooks")
                    .and_then(|h| h.as_array())
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_git_ai_augment_command)
                    .unwrap_or(false)
            })
            .count()
    }

    // ---- is_git_ai_augment_command ----

    #[test]
    fn test_is_git_ai_augment_command_matches() {
        assert!(is_git_ai_augment_command(
            "/usr/local/bin/git-ai checkpoint augment --hook-input stdin"
        ));
        assert!(is_git_ai_augment_command(
            "git-ai checkpoint augment --hook-input stdin"
        ));
    }

    #[test]
    fn test_is_git_ai_augment_command_does_not_match_siblings() {
        // Must not match other git-ai presets.
        assert!(!is_git_ai_augment_command(
            "git-ai checkpoint claude --hook-input stdin"
        ));
        assert!(!is_git_ai_augment_command(
            "git-ai checkpoint augment-pro --hook-input stdin"
        ));
        assert!(!is_git_ai_augment_command(
            "git-ai checkpoint augment2 --hook-input stdin"
        ));
        assert!(!is_git_ai_augment_command("echo unrelated"));
    }

    // ---- Install scenarios ----

    #[test]
    fn s1_fresh_install_creates_pre_and_post() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "fresh install should produce a diff");

        for event in &AUGMENT_HOOK_EVENTS {
            let blocks = read_event_blocks(&path, event);
            assert_eq!(blocks.len(), 1, "{event} should have one matcher block");
            assert_eq!(
                blocks[0].get("matcher").and_then(|m| m.as_str()).unwrap(),
                AUGMENT_CATCH_ALL_MATCHER
            );
            let inner = blocks[0]
                .get("hooks")
                .and_then(|h| h.as_array())
                .cloned()
                .unwrap_or_default();
            assert_eq!(inner.len(), 1);
            assert_eq!(
                inner[0].get("command").and_then(|c| c.as_str()).unwrap(),
                expected_cmd()
            );
            assert_eq!(
                inner[0].get("type").and_then(|t| t.as_str()).unwrap(),
                "command"
            );
        }
    }

    #[test]
    fn s2_idempotent_already_installed() {
        let (_td, path) = setup_test_env();
        AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        let diff2 = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff2.is_none(), "second install should be a no-op");
    }

    #[test]
    fn s3_preserves_unrelated_hooks_and_other_settings() {
        let (_td, path) = setup_test_env();
        let unrelated = r#"{
  "model": "claude-sonnet-4-5",
  "permissions": { "allowList": [] },
  "hooks": {
    "PreToolUse": [
      { "matcher": "launch-process",
        "hooks": [{"type": "command", "command": "echo unrelated"}] }
    ]
  }
}"#;
        fs::write(&path, unrelated).unwrap();

        AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // Original settings preserved.
        assert!(content.contains("\"model\""), "{content}");
        assert!(content.contains("claude-sonnet-4-5"), "{content}");
        assert!(content.contains("permissions"), "{content}");
        assert!(content.contains("echo unrelated"), "{content}");
        // Our entries added.
        let pre_blocks = read_event_blocks(&path, "PreToolUse");
        let total_git_ai = count_git_ai_entries(&pre_blocks);
        assert_eq!(total_git_ai, 1, "exactly one git-ai entry under PreToolUse");
    }

    #[test]
    fn s4_updates_outdated_command_path() {
        let (_td, path) = setup_test_env();
        let stale = r#"{
  "hooks": {
    "PreToolUse": [
      { "matcher": ".*",
        "hooks": [{"type": "command", "command": "/old/path/git-ai checkpoint augment --hook-input stdin"}] }
    ],
    "PostToolUse": [
      { "matcher": ".*",
        "hooks": [{"type": "command", "command": "/old/path/git-ai checkpoint augment --hook-input stdin"}] }
    ]
  }
}"#;
        fs::write(&path, stale).unwrap();

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "stale path should produce a diff");

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("/usr/local/bin/git-ai"), "{content}");
        assert!(!content.contains("/old/path/git-ai"), "{content}");

        for event in &AUGMENT_HOOK_EVENTS {
            assert_eq!(count_git_ai_entries(&read_event_blocks(&path, event)), 1);
        }
    }

    #[test]
    fn s5_dedups_existing_augment_entries_for_same_event() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let dup = format!(
            r#"{{
  "hooks": {{
    "PreToolUse": [
      {{ "matcher": ".*",
        "hooks": [
          {{"type": "command", "command": "{cmd}"}},
          {{"type": "command", "command": "{cmd}"}}
        ] }}
    ]
  }}
}}"#
        );
        fs::write(&path, dup).unwrap();

        AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let blocks = read_event_blocks(&path, "PreToolUse");
        assert_eq!(count_git_ai_entries(&blocks), 1, "duplicates collapsed");
    }

    #[test]
    fn s6_migrates_git_ai_from_non_catch_all_matcher() {
        // A previous bad install or manual edit dropped our entry into a
        // tool-specific matcher block. Install should migrate it to the
        // catch-all and remove the now-empty tool-specific block.
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let migrate = format!(
            r#"{{
  "hooks": {{
    "PreToolUse": [
      {{ "matcher": "launch-process",
        "hooks": [{{"type": "command", "command": "{cmd}"}}] }}
    ]
  }}
}}"#
        );
        fs::write(&path, migrate).unwrap();

        AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let blocks = read_event_blocks(&path, "PreToolUse");
        // Now exactly one block (the catch-all), with our entry.
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].get("matcher").and_then(|m| m.as_str()).unwrap(),
            AUGMENT_CATCH_ALL_MATCHER
        );
        assert_eq!(count_git_ai_entries(&blocks), 1);
    }

    #[test]
    fn s7_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), true).unwrap();
        assert!(diff.is_some(), "dry run still computes a diff");
        assert!(!path.exists(), "dry run must not write the file");
    }

    #[test]
    fn s8_create_dir_on_first_install() {
        let td = TempDir::new().unwrap();
        let nested = td
            .path()
            .join("custom")
            .join(".augment")
            .join("settings.json");
        assert!(!nested.parent().unwrap().exists());
        AugmentInstaller::install_hooks_at(&nested, &params(), false).unwrap();
        assert!(nested.exists());
    }

    // ---- Uninstall scenarios ----

    #[test]
    fn u1_uninstall_removes_only_augment_entries() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let mixed = format!(
            r#"{{
  "hooks": {{
    "PreToolUse": [
      {{ "matcher": ".*",
        "hooks": [
          {{"type": "command", "command": "{cmd}"}},
          {{"type": "command", "command": "echo not ours"}}
        ] }}
    ]
  }}
}}"#
        );
        fs::write(&path, mixed).unwrap();

        let diff = AugmentInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_some());

        let blocks = read_event_blocks(&path, "PreToolUse");
        assert_eq!(count_git_ai_entries(&blocks), 0);
        // User entry survived.
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo not ours"), "{content}");
    }

    #[test]
    fn u2_uninstall_returns_none_when_no_augment_entries() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            r#"{"hooks": {"PreToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "echo unrelated"}]}]}}"#,
        )
        .unwrap();
        let diff = AugmentInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u3_uninstall_returns_none_when_settings_missing() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();
        let diff = AugmentInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u4_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let initial = format!(
            r#"{{"hooks": {{"PreToolUse": [{{"matcher": ".*", "hooks": [{{"type": "command", "command": "{cmd}"}}]}}]}}}}"#
        );
        fs::write(&path, &initial).unwrap();

        let diff = AugmentInstaller::uninstall_hooks_at(&path, true).unwrap();
        assert!(diff.is_some());
        // File contents unchanged.
        assert_eq!(fs::read_to_string(&path).unwrap(), initial);
    }

    // ---- Error handling ----

    #[test]
    fn e1_invalid_json_install_errors() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "{not valid json}").unwrap();
        let result = AugmentInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to parse Augment settings.json"),
            "{msg}"
        );
    }

    #[test]
    fn e2_invalid_json_uninstall_returns_none() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "[not valid].").unwrap();
        let diff = AugmentInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn e3_root_must_be_object() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "[]").unwrap();
        let result = AugmentInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
    }

    // ---- hook_status (check_hooks helper) ----

    #[test]
    fn check_status_reports_installed_when_present_in_catch_all() {
        let cmd = expected_cmd();
        let v: Value = serde_json::from_str(&format!(
            r#"{{"hooks": {{
                "PreToolUse": [{{"matcher": ".*", "hooks": [{{"type": "command", "command": "{cmd}"}}]}}],
                "PostToolUse": [{{"matcher": ".*", "hooks": [{{"type": "command", "command": "{cmd}"}}]}}]
            }}}}"#
        ))
        .unwrap();
        let (installed, up_to_date) = AugmentInstaller::hook_status(&v, &cmd);
        assert!(installed);
        assert!(up_to_date);
    }

    #[test]
    fn check_status_reports_outdated_when_only_one_event_present() {
        let cmd = expected_cmd();
        let v: Value = serde_json::from_str(&format!(
            r#"{{"hooks": {{
                "PreToolUse": [{{"matcher": ".*", "hooks": [{{"type": "command", "command": "{cmd}"}}]}}]
            }}}}"#
        ))
        .unwrap();
        let (installed, up_to_date) = AugmentInstaller::hook_status(&v, &cmd);
        assert!(installed);
        assert!(!up_to_date);
    }
}
