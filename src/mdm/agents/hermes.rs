//! Hook installer for Hermes (NousResearch hermes-agent).
//!
//! Hermes is a Python-based AI agent. Shell hooks are configured in
//! `~/.hermes/config.yaml` per the upstream docs at
//! <https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks>:
//!
//! ```yaml
//! hooks:
//!   pre_tool_call:
//!     - matcher: ".*"
//!       command: "/path/to/git-ai checkpoint hermes --hook-input stdin"
//!       timeout: 60
//!   post_tool_call:
//!     - matcher: ".*"
//!       command: "/path/to/git-ai checkpoint hermes --hook-input stdin"
//!       timeout: 60
//! hooks_auto_accept: true
//! ```
//!
//! We install one entry under the `".*"` matcher (regex match-all per
//! Hermes' docs) for each of `pre_tool_call` and `post_tool_call`. The
//! preset itself filters tool events to the tools we actually
//! checkpoint (`write_file`, `patch`, `terminal`).
//!
//! Because Hermes prompts the user for first-use consent on every
//! `(event, command)` pair, we also set `hooks_auto_accept: true` to
//! suppress the prompt for our installed entries. Users who set this to
//! `false` themselves will see a one-time prompt the first time the
//! agent runs after install. Re-running the installer is idempotent:
//! the YAML is compared structurally so whitespace and key order
//! changes don't trigger spurious diffs.
//!
//! Only entries we own (matched via `is_git_ai_hermes_command`) are
//! touched on uninstall, so user-defined hooks survive.

use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic,
};
use serde_yml::{Mapping, Value};
use std::fs;
use std::path::{Path, PathBuf};

const HERMES_CHECKPOINT_CMD: &str = "checkpoint hermes --hook-input stdin";
const HERMES_HOOK_EVENTS: [&str; 2] = ["pre_tool_call", "post_tool_call"];
const HERMES_CATCH_ALL_MATCHER: &str = ".*";
const HERMES_DEFAULT_TIMEOUT: i64 = 60;

/// Returns true only for git-ai hooks belonging to *this* preset
/// (`git-ai checkpoint hermes ...`). The shared
/// `is_git_ai_checkpoint_command` helper matches any
/// `git-ai checkpoint <preset>` line; we further verify the preset name
/// is exactly `hermes` (not a substring) by inspecting whitespace
/// tokens.
fn is_git_ai_hermes_command(cmd: &str) -> bool {
    if !is_git_ai_checkpoint_command(cmd) {
        return false;
    }
    let mut tokens = cmd.split_whitespace();
    while let Some(t) = tokens.next() {
        if t == "checkpoint"
            && let Some(name) = tokens.next()
        {
            return name == "hermes";
        }
    }
    false
}

pub struct HermesInstaller;

impl HermesInstaller {
    fn config_dir() -> PathBuf {
        home_dir().join(".hermes")
    }

    fn config_path() -> PathBuf {
        Self::config_dir().join("config.yaml")
    }

    fn desired_command(binary_path: &Path) -> String {
        format!("{} {}", binary_path.display(), HERMES_CHECKPOINT_CMD)
    }

    /// Returns `(hooks_installed, hooks_up_to_date)` from a parsed YAML root.
    /// `hooks_installed` = a git-ai-hermes entry exists for at least one event.
    /// `hooks_up_to_date` = an entry exists for every event we install.
    fn hook_status(config: &Value, desired_cmd: &str) -> (bool, bool) {
        let Some(hooks_map) = config.get("hooks").and_then(|h| h.as_mapping()) else {
            return (false, false);
        };

        let mut hooks_installed = false;
        let mut up_to_date_events: Vec<&str> = Vec::new();

        for event in &HERMES_HOOK_EVENTS {
            let Some(entries) = hooks_map
                .get(Value::from(*event))
                .and_then(|v| v.as_sequence())
            else {
                continue;
            };
            for entry in entries {
                let Some(cmd) = entry.get("command").and_then(|v| v.as_str()) else {
                    continue;
                };
                if !is_git_ai_hermes_command(cmd) {
                    continue;
                }
                hooks_installed = true;
                if cmd == desired_cmd && !up_to_date_events.contains(event) {
                    up_to_date_events.push(event);
                }
            }
        }

        let hooks_up_to_date = HERMES_HOOK_EVENTS
            .iter()
            .all(|e| up_to_date_events.contains(e));
        (hooks_installed, hooks_up_to_date)
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

        let existing: Value = if existing_content.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yml::from_str(&existing_content).map_err(|e| {
                GitAiError::Generic(format!("Failed to parse Hermes config.yaml: {e}"))
            })?
        };

        if !existing.is_mapping() {
            return Err(GitAiError::Generic(
                "Hermes config.yaml root must be a YAML mapping".to_string(),
            ));
        }

        let desired_cmd = Self::desired_command(&params.binary_path);

        let mut merged = existing.clone();
        let merged_map = merged.as_mapping_mut().expect("checked is_mapping above");

        // Get/create the hooks mapping.
        let hooks_value = merged_map
            .entry(Value::from("hooks"))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if !hooks_value.is_mapping() {
            return Err(GitAiError::Generic(
                "Hermes config.yaml `hooks` field must be a mapping".to_string(),
            ));
        }
        let hooks_map = hooks_value
            .as_mapping_mut()
            .expect("checked is_mapping above");

        for event in &HERMES_HOOK_EVENTS {
            let event_key = Value::from(*event);
            let event_value = hooks_map
                .entry(event_key.clone())
                .or_insert_with(|| Value::Sequence(Vec::new()));
            if !event_value.is_sequence() {
                return Err(GitAiError::Generic(format!(
                    "Hermes config.yaml `hooks.{event}` field must be a sequence",
                )));
            }
            let event_array = event_value
                .as_sequence_mut()
                .expect("checked is_sequence above");

            // Step 1: find FIRST git-ai-hermes entry, update if needed,
            // dedup the rest.
            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;
            for (idx, entry) in event_array.iter().enumerate() {
                if let Some(cmd) = entry.get("command").and_then(|v| v.as_str())
                    && is_git_ai_hermes_command(cmd)
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
                    if needs_update && let Some(table) = event_array[idx].as_mapping_mut() {
                        table.insert(Value::from("command"), Value::from(desired_cmd.clone()));
                    }
                    let keep_idx = idx;
                    let mut current = 0usize;
                    event_array.retain(|entry| {
                        if current == keep_idx {
                            current += 1;
                            true
                        } else if let Some(cmd) = entry.get("command").and_then(|v| v.as_str()) {
                            let dup = is_git_ai_hermes_command(cmd);
                            current += 1;
                            !dup
                        } else {
                            current += 1;
                            true
                        }
                    });
                }
                None => {
                    let mut entry = Mapping::new();
                    entry.insert(
                        Value::from("matcher"),
                        Value::from(HERMES_CATCH_ALL_MATCHER),
                    );
                    entry.insert(Value::from("command"), Value::from(desired_cmd.clone()));
                    entry.insert(Value::from("timeout"), Value::from(HERMES_DEFAULT_TIMEOUT));
                    event_array.push(Value::Mapping(entry));
                }
            }
        }

        // Set hooks_auto_accept: true at the root so our entries don't
        // trigger first-use consent prompts. Only set it if not already
        // present (respect the user's choice if they've explicitly set
        // it to false).
        if !merged_map.contains_key(Value::from("hooks_auto_accept")) {
            merged_map.insert(Value::from("hooks_auto_accept"), Value::from(true));
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_yml::to_string(&merged).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Hermes config.yaml: {e}"))
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
        let existing: Value = match serde_yml::from_str(&existing_content) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };

        let mut merged = existing.clone();
        let Some(merged_map) = merged.as_mapping_mut() else {
            return Ok(None);
        };

        let Some(hooks_value) = merged_map.get_mut(Value::from("hooks")) else {
            return Ok(None);
        };
        let Some(hooks_map) = hooks_value.as_mapping_mut() else {
            return Ok(None);
        };

        let mut changed = false;
        let mut empty_events: Vec<Value> = Vec::new();

        for event in &HERMES_HOOK_EVENTS {
            let event_key = Value::from(*event);
            if let Some(event_value) = hooks_map.get_mut(&event_key)
                && let Some(event_array) = event_value.as_sequence_mut()
            {
                let original = event_array.len();
                event_array.retain(|entry| {
                    entry
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(|cmd| !is_git_ai_hermes_command(cmd))
                        .unwrap_or(true)
                });
                if event_array.len() != original {
                    changed = true;
                }
                if event_array.is_empty() {
                    empty_events.push(event_key);
                }
            }
        }

        // Remove emptied event keys so the file isn't littered with
        // dangling `pre_tool_call: []`.
        for key in &empty_events {
            hooks_map.remove(key);
        }

        // Remove the hooks key entirely if it's now empty.
        if hooks_map.is_empty() {
            merged_map.remove(Value::from("hooks"));
        }

        if !changed {
            return Ok(None);
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_yml::to_string(&merged).map_err(|e| {
            GitAiError::Generic(format!("Failed to serialize Hermes config.yaml: {e}"))
        })?;

        let diff_output = generate_diff(config_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(config_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}

impl HookInstaller for HermesInstaller {
    fn name(&self) -> &str {
        "Hermes"
    }

    fn id(&self) -> &str {
        "hermes"
    }

    fn process_names(&self) -> Vec<&str> {
        // Hermes ships three entry points per pyproject.toml's
        // [project.scripts]: `hermes` (primary CLI), `hermes-agent`
        // (agent runner), `hermes-acp` (ACP server for VS Code/Zed/JetBrains).
        vec!["hermes", "hermes-agent", "hermes-acp"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary =
            binary_exists("hermes") || binary_exists("hermes-agent") || binary_exists("hermes-acp");
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
        let parsed: Value = match serde_yml::from_str(&content) {
            Ok(v) => v,
            Err(_) => Value::Mapping(Mapping::new()),
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
        let td = TempDir::new().unwrap();
        let path = td.path().join(".hermes").join("config.yaml");
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
        format!("{} {}", binary_path().display(), HERMES_CHECKPOINT_CMD)
    }

    fn read_config(path: &Path) -> Value {
        let content = fs::read_to_string(path).unwrap();
        serde_yml::from_str(&content).unwrap()
    }

    fn count_hermes_entries(config: &Value, event: &str) -> usize {
        config
            .get("hooks")
            .and_then(|h| h.get(event))
            .and_then(|v| v.as_sequence())
            .map(|arr| {
                arr.iter()
                    .filter(|entry| {
                        entry
                            .get("command")
                            .and_then(|v| v.as_str())
                            .map(is_git_ai_hermes_command)
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    // ---- is_git_ai_hermes_command ----

    #[test]
    fn test_is_git_ai_hermes_command_matches() {
        assert!(is_git_ai_hermes_command(
            "/usr/local/bin/git-ai checkpoint hermes --hook-input stdin"
        ));
        assert!(is_git_ai_hermes_command(
            "git-ai checkpoint hermes --hook-input stdin"
        ));
    }

    #[test]
    fn test_is_git_ai_hermes_command_does_not_match_siblings() {
        assert!(!is_git_ai_hermes_command(
            "git-ai checkpoint claude --hook-input stdin"
        ));
        assert!(!is_git_ai_hermes_command(
            "git-ai checkpoint hermes-pro --hook-input stdin"
        ));
        assert!(!is_git_ai_hermes_command(
            "git-ai checkpoint hermes2 --hook-input stdin"
        ));
        assert!(!is_git_ai_hermes_command("echo unrelated"));
    }

    // ---- Install scenarios ----

    #[test]
    fn s1_fresh_install_creates_pre_and_post() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = HermesInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "fresh install should produce a diff");

        let cfg = read_config(&path);
        for event in &HERMES_HOOK_EVENTS {
            assert_eq!(count_hermes_entries(&cfg, event), 1);
            let entries = cfg
                .get("hooks")
                .and_then(|h| h.get(event))
                .and_then(|v| v.as_sequence())
                .unwrap();
            assert_eq!(
                entries[0].get("matcher").and_then(|v| v.as_str()).unwrap(),
                HERMES_CATCH_ALL_MATCHER
            );
            assert_eq!(
                entries[0].get("command").and_then(|v| v.as_str()).unwrap(),
                expected_cmd()
            );
            assert_eq!(
                entries[0].get("timeout").and_then(|v| v.as_i64()).unwrap(),
                HERMES_DEFAULT_TIMEOUT
            );
        }
        assert!(
            cfg.get("hooks_auto_accept")
                .and_then(|v| v.as_bool())
                .unwrap()
        );
    }

    #[test]
    fn s2_idempotent_already_installed() {
        let (_td, path) = setup_test_env();
        HermesInstaller::install_hooks_at(&path, &params(), false).unwrap();
        let diff2 = HermesInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff2.is_none(), "second install should be a no-op");
    }

    #[test]
    fn s3_preserves_unrelated_hooks_and_other_settings() {
        let (_td, path) = setup_test_env();
        let unrelated = r#"model: gpt-5
api_key: sk-xxx
hooks:
  pre_tool_call:
    - matcher: terminal
      command: ~/.hermes/agent-hooks/block-rm-rf.sh
      timeout: 5
"#;
        fs::write(&path, unrelated).unwrap();

        HermesInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // Original settings preserved.
        assert!(content.contains("gpt-5"), "{content}");
        assert!(content.contains("sk-xxx"), "{content}");
        assert!(content.contains("block-rm-rf"), "{content}");

        // Our entries added.
        let cfg = read_config(&path);
        assert_eq!(count_hermes_entries(&cfg, "pre_tool_call"), 1);
        assert_eq!(count_hermes_entries(&cfg, "post_tool_call"), 1);
    }

    #[test]
    fn s4_updates_outdated_command_path() {
        let (_td, path) = setup_test_env();
        let stale = r#"hooks:
  pre_tool_call:
    - matcher: ".*"
      command: /old/path/git-ai checkpoint hermes --hook-input stdin
  post_tool_call:
    - matcher: ".*"
      command: /old/path/git-ai checkpoint hermes --hook-input stdin
"#;
        fs::write(&path, stale).unwrap();

        let diff = HermesInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "stale path should produce a diff");

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("/usr/local/bin/git-ai"), "{content}");
        assert!(!content.contains("/old/path/git-ai"), "{content}");

        let cfg = read_config(&path);
        for event in &HERMES_HOOK_EVENTS {
            assert_eq!(count_hermes_entries(&cfg, event), 1);
        }
    }

    #[test]
    fn s5_dedups_existing_hermes_entries_for_same_event() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let dup = format!(
            r#"hooks:
  pre_tool_call:
    - matcher: ".*"
      command: {cmd}
    - matcher: ".*"
      command: {cmd}
"#
        );
        fs::write(&path, dup).unwrap();

        HermesInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let cfg = read_config(&path);
        assert_eq!(count_hermes_entries(&cfg, "pre_tool_call"), 1);
    }

    #[test]
    fn s6_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = HermesInstaller::install_hooks_at(&path, &params(), true).unwrap();
        assert!(diff.is_some(), "dry run still computes a diff");
        assert!(!path.exists(), "dry run must not write the file");
    }

    #[test]
    fn s7_create_dir_on_first_install() {
        let td = TempDir::new().unwrap();
        let nested = td.path().join("custom").join(".hermes").join("config.yaml");
        assert!(!nested.parent().unwrap().exists());
        HermesInstaller::install_hooks_at(&nested, &params(), false).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn s8_respects_user_explicit_hooks_auto_accept_false() {
        let (_td, path) = setup_test_env();
        let initial = "hooks_auto_accept: false\n";
        fs::write(&path, initial).unwrap();

        HermesInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let cfg = read_config(&path);
        assert!(
            !cfg.get("hooks_auto_accept")
                .and_then(|v| v.as_bool())
                .unwrap(),
            "User's explicit `hooks_auto_accept: false` must not be overwritten"
        );
    }

    // ---- Uninstall scenarios ----

    #[test]
    fn u1_uninstall_removes_only_hermes_entries() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let mixed = format!(
            r#"hooks:
  pre_tool_call:
    - matcher: ".*"
      command: {cmd}
    - matcher: terminal
      command: echo not ours
"#
        );
        fs::write(&path, mixed).unwrap();

        let diff = HermesInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_some());

        let cfg = read_config(&path);
        assert_eq!(count_hermes_entries(&cfg, "pre_tool_call"), 0);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo not ours"), "{content}");
    }

    #[test]
    fn u2_uninstall_returns_none_when_no_hermes_entries() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            "hooks:\n  pre_tool_call:\n    - matcher: terminal\n      command: echo unrelated\n",
        )
        .unwrap();
        let diff = HermesInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u3_uninstall_returns_none_when_config_missing() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();
        let diff = HermesInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u4_uninstall_removes_emptied_event_keys() {
        // Uninstalling our entries should remove the now-empty
        // pre_tool_call/post_tool_call keys (and the empty hooks key)
        // rather than leaving dangling [].
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let only_ours = format!(
            r#"hooks:
  pre_tool_call:
    - matcher: ".*"
      command: {cmd}
  post_tool_call:
    - matcher: ".*"
      command: {cmd}
"#
        );
        fs::write(&path, only_ours).unwrap();

        HermesInstaller::uninstall_hooks_at(&path, false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("pre_tool_call"),
            "empty event key should be removed; got: {content}"
        );
        assert!(
            !content.contains("hooks"),
            "empty hooks block should be removed; got: {content}"
        );
    }

    #[test]
    fn u5_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let initial =
            format!("hooks:\n  pre_tool_call:\n    - matcher: \".*\"\n      command: {cmd}\n");
        fs::write(&path, &initial).unwrap();

        let diff = HermesInstaller::uninstall_hooks_at(&path, true).unwrap();
        assert!(diff.is_some());
        assert_eq!(fs::read_to_string(&path).unwrap(), initial);
    }

    // ---- Error handling ----

    #[test]
    fn e1_invalid_yaml_install_errors() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "this : : is not valid yaml ::: at all").unwrap();
        let result = HermesInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Failed to parse Hermes config.yaml"), "{msg}");
    }

    #[test]
    fn e2_invalid_yaml_uninstall_returns_none() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "%%% : invalid yaml :::").unwrap();
        let diff = HermesInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn e3_root_must_be_mapping() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "- 1\n- 2\n").unwrap();
        let result = HermesInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
    }

    #[test]
    fn e4_hooks_must_be_mapping() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "hooks: not_a_mapping\n").unwrap();
        let result = HermesInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
    }

    // ---- hook_status ----

    #[test]
    fn check_status_reports_installed_when_present() {
        let cmd = expected_cmd();
        let v: Value = serde_yml::from_str(&format!(
            r#"hooks:
  pre_tool_call:
    - matcher: ".*"
      command: {cmd}
  post_tool_call:
    - matcher: ".*"
      command: {cmd}
"#
        ))
        .unwrap();
        let (installed, up_to_date) = HermesInstaller::hook_status(&v, &cmd);
        assert!(installed);
        assert!(up_to_date);
    }
}
