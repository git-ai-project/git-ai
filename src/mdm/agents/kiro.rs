//! Hook installer for Kiro CLI (`kiro-cli`).
//!
//! Kiro CLI's hook system is **per-agent JSON files** rather than a
//! single global config. Hooks live under the `hooks` field of an
//! agent JSON file at:
//!
//! - Global agents: `~/.kiro/agents/<name>.json`
//! - Project-local agents (override): `.kiro/agents/<name>.json`
//!
//! Per the upstream docs at
//! <https://kiro.dev/docs/cli/custom-agents/configuration-reference/>:
//!
//! ```json
//! {
//!   "hooks": {
//!     "preToolUse":  [{"matcher": "execute_bash", "command": "..."}],
//!     "postToolUse": [{"matcher": "fs_write",     "command": "..."}]
//!   }
//! }
//! ```
//!
//! Kiro does not document a "default" agent file name, so we install
//! into `~/.kiro/agents/default.json`, creating it as a fresh agent
//! file if missing. Users who run multiple custom agents will need to
//! either re-install per-agent or symlink — documented as a known
//! limitation.
//!
//! We install entries with no `matcher` (which Kiro treats as
//! "match all") for each of `preToolUse` and `postToolUse`. The
//! preset itself filters to the tools we actually checkpoint.
//!
//! The installer is idempotent: re-running it leaves the JSON
//! unchanged when our entries are already present and current. Only
//! entries we own (matched via `is_git_ai_kiro_command`) are touched
//! on uninstall, so user-defined hooks survive.

use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const KIRO_CHECKPOINT_CMD: &str = "checkpoint kiro --hook-input stdin";
const KIRO_HOOK_EVENTS: [&str; 2] = ["preToolUse", "postToolUse"];
/// Name we set on the freshly-created agent JSON.  Mirrors the file
/// stem (`default.json`) and matches the upstream
/// `DEFAULT_AGENT_NAME` constant in `aws/amazon-q-developer-cli`.
const KIRO_DEFAULT_AGENT_NAME: &str = "default";

/// Returns true only for git-ai hooks belonging to *this* preset
/// (`git-ai checkpoint kiro ...`). The shared
/// `is_git_ai_checkpoint_command` helper matches any
/// `git-ai checkpoint <preset>` line; we further verify the preset
/// name is exactly `kiro` (not a substring) by inspecting whitespace
/// tokens.
fn is_git_ai_kiro_command(cmd: &str) -> bool {
    if !is_git_ai_checkpoint_command(cmd) {
        return false;
    }
    let mut tokens = cmd.split_whitespace();
    while let Some(t) = tokens.next() {
        if t == "checkpoint"
            && let Some(name) = tokens.next()
        {
            return name == "kiro";
        }
    }
    false
}

pub struct KiroInstaller;

impl KiroInstaller {
    fn config_dir() -> PathBuf {
        home_dir().join(".kiro")
    }

    fn agents_dir() -> PathBuf {
        Self::config_dir().join("agents")
    }

    fn default_agent_path() -> PathBuf {
        Self::agents_dir().join("default.json")
    }

    fn desired_command(binary_path: &Path) -> String {
        format!("{} {}", binary_path.display(), KIRO_CHECKPOINT_CMD)
    }

    /// Returns `(hooks_installed, hooks_up_to_date)` from a parsed agent JSON.
    fn hook_status(agent: &Value, desired_cmd: &str) -> (bool, bool) {
        let Some(hooks) = agent.get("hooks").and_then(|h| h.as_object()) else {
            return (false, false);
        };

        let mut hooks_installed = false;
        let mut up_to_date_events: Vec<&str> = Vec::new();

        for event in &KIRO_HOOK_EVENTS {
            let Some(entries) = hooks.get(*event).and_then(|v| v.as_array()) else {
                continue;
            };
            for entry in entries {
                let Some(cmd) = entry.get("command").and_then(|v| v.as_str()) else {
                    continue;
                };
                if !is_git_ai_kiro_command(cmd) {
                    continue;
                }
                hooks_installed = true;
                if cmd == desired_cmd && !up_to_date_events.contains(event) {
                    up_to_date_events.push(event);
                }
            }
        }

        let hooks_up_to_date = KIRO_HOOK_EVENTS
            .iter()
            .all(|e| up_to_date_events.contains(e));
        (hooks_installed, hooks_up_to_date)
    }

    fn install_hooks_at(
        agent_path: &Path,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if let Some(dir) = agent_path.parent() {
            fs::create_dir_all(dir)?;
        }

        let existing_content = if agent_path.exists() {
            fs::read_to_string(agent_path)?
        } else {
            String::new()
        };

        let existing: Value = if existing_content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing_content)
                .map_err(|e| GitAiError::Generic(format!("Failed to parse Kiro agent JSON: {e}")))?
        };

        if !existing.is_object() {
            return Err(GitAiError::Generic(
                "Kiro agent JSON root must be a JSON object".to_string(),
            ));
        }

        let desired_cmd = Self::desired_command(&params.binary_path);

        let mut merged = existing.clone();

        // The upstream `AgentConfigV2025_08_22` struct (in
        // `aws/amazon-q-developer-cli`) requires `name` as a non-default
        // field. When creating a fresh agent file, populate it so Kiro
        // can deserialize the result. Existing user files keep whatever
        // name is already there.
        if let Some(root) = merged.as_object_mut()
            && !root.contains_key("name")
        {
            root.insert(
                "name".to_string(),
                Value::String(KIRO_DEFAULT_AGENT_NAME.to_string()),
            );
        }

        let mut hooks_value = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));
        if !hooks_value.is_object() {
            return Err(GitAiError::Generic(
                "Kiro agent JSON `hooks` field must be a JSON object".to_string(),
            ));
        }

        for event in &KIRO_HOOK_EVENTS {
            let mut event_array = hooks_value
                .get(*event)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Find the FIRST git-ai-kiro entry, update if needed,
            // dedup the rest.
            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;
            for (idx, entry) in event_array.iter().enumerate() {
                if let Some(cmd) = entry.get("command").and_then(|v| v.as_str())
                    && is_git_ai_kiro_command(cmd)
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
                    if needs_update && let Some(table) = event_array[idx].as_object_mut() {
                        table.insert("command".to_string(), Value::String(desired_cmd.clone()));
                    }
                    let keep_idx = idx;
                    let mut current = 0usize;
                    event_array.retain(|entry| {
                        if current == keep_idx {
                            current += 1;
                            true
                        } else if let Some(cmd) = entry.get("command").and_then(|v| v.as_str()) {
                            let dup = is_git_ai_kiro_command(cmd);
                            current += 1;
                            !dup
                        } else {
                            current += 1;
                            true
                        }
                    });
                }
                None => {
                    event_array.push(json!({
                        "command": desired_cmd,
                    }));
                }
            }

            if let Some(obj) = hooks_value.as_object_mut() {
                obj.insert(event.to_string(), Value::Array(event_array));
            }
        }

        if let Some(root) = merged.as_object_mut() {
            root.insert("hooks".to_string(), hooks_value);
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(agent_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(agent_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }

    fn uninstall_hooks_at(agent_path: &Path, dry_run: bool) -> Result<Option<String>, GitAiError> {
        if !agent_path.exists() {
            return Ok(None);
        }

        let existing_content = fs::read_to_string(agent_path)?;
        let existing: Value = match serde_json::from_str(&existing_content) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };

        let mut merged = existing.clone();
        let Some(merged_map) = merged.as_object_mut() else {
            return Ok(None);
        };

        let Some(hooks_value) = merged_map.get_mut("hooks") else {
            return Ok(None);
        };
        let Some(hooks_obj) = hooks_value.as_object_mut() else {
            return Ok(None);
        };

        let mut changed = false;
        let mut empty_keys: Vec<String> = Vec::new();

        for event in &KIRO_HOOK_EVENTS {
            if let Some(event_value) = hooks_obj.get_mut(*event)
                && let Some(event_array) = event_value.as_array_mut()
            {
                let original = event_array.len();
                event_array.retain(|entry| {
                    entry
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(|cmd| !is_git_ai_kiro_command(cmd))
                        .unwrap_or(true)
                });
                if event_array.len() != original {
                    changed = true;
                }
                if event_array.is_empty() {
                    empty_keys.push((*event).to_string());
                }
            }
        }

        for key in &empty_keys {
            hooks_obj.remove(key);
        }

        if hooks_obj.is_empty() {
            merged_map.remove("hooks");
        }

        if !changed {
            return Ok(None);
        }

        if existing == merged {
            return Ok(None);
        }

        let new_content = serde_json::to_string_pretty(&merged)?;
        let diff_output = generate_diff(agent_path, &existing_content, &new_content);

        if !dry_run {
            write_atomic(agent_path, new_content.as_bytes())?;
        }

        Ok(Some(diff_output))
    }
}

impl HookInstaller for KiroInstaller {
    fn name(&self) -> &str {
        "Kiro"
    }

    fn id(&self) -> &str {
        "kiro"
    }

    fn process_names(&self) -> Vec<&str> {
        // The CLI binary is `kiro-cli` per the docs; the IDE binary
        // (`kiro`) does not have a documented payload schema, so we
        // intentionally only target the CLI.
        vec!["kiro-cli"]
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_binary = binary_exists("kiro-cli");
        let has_dotfiles = Self::config_dir().exists();

        if !has_binary && !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let agent_path = Self::default_agent_path();
        if !agent_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(&agent_path)?;
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
        Self::install_hooks_at(&Self::default_agent_path(), params, dry_run)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Self::uninstall_hooks_at(&Self::default_agent_path(), dry_run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn binary_path() -> PathBuf {
        PathBuf::from("/usr/local/bin/git-ai")
    }

    fn params() -> HookInstallerParams {
        HookInstallerParams {
            binary_path: binary_path(),
        }
    }

    fn expected_cmd() -> String {
        format!("{} {}", binary_path().display(), KIRO_CHECKPOINT_CMD)
    }

    fn setup_test_env() -> (TempDir, PathBuf) {
        let td = TempDir::new().unwrap();
        let path = td.path().join(".kiro").join("agents").join("default.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        (td, path)
    }

    fn read_agent(path: &Path) -> Value {
        let content = fs::read_to_string(path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    fn count_kiro_entries(agent: &Value, event: &str) -> usize {
        agent
            .get("hooks")
            .and_then(|h| h.get(event))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|entry| {
                        entry
                            .get("command")
                            .and_then(|v| v.as_str())
                            .map(is_git_ai_kiro_command)
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    // ---- is_git_ai_kiro_command ----

    #[test]
    fn test_is_git_ai_kiro_command_matches() {
        assert!(is_git_ai_kiro_command(
            "/usr/local/bin/git-ai checkpoint kiro --hook-input stdin"
        ));
        assert!(is_git_ai_kiro_command(
            "git-ai checkpoint kiro --hook-input stdin"
        ));
    }

    #[test]
    fn test_is_git_ai_kiro_command_does_not_match_siblings() {
        assert!(!is_git_ai_kiro_command(
            "git-ai checkpoint claude --hook-input stdin"
        ));
        assert!(!is_git_ai_kiro_command(
            "git-ai checkpoint kiro-pro --hook-input stdin"
        ));
        assert!(!is_git_ai_kiro_command(
            "git-ai checkpoint kiro2 --hook-input stdin"
        ));
        assert!(!is_git_ai_kiro_command("echo unrelated"));
    }

    // ---- Install ----

    #[test]
    fn s1_fresh_install_creates_pre_and_post() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = KiroInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "fresh install should produce a diff");

        let cfg = read_agent(&path);

        // Required `name` field per upstream `AgentConfigV2025_08_22`
        // schema in aws/amazon-q-developer-cli — without this, Kiro
        // refuses to deserialize the agent file.
        assert_eq!(
            cfg.get("name").and_then(|v| v.as_str()),
            Some(KIRO_DEFAULT_AGENT_NAME),
            "fresh agent file must include the upstream-required `name` field"
        );

        for event in &KIRO_HOOK_EVENTS {
            assert_eq!(count_kiro_entries(&cfg, event), 1);
            let entries = cfg
                .get("hooks")
                .and_then(|h| h.get(event))
                .and_then(|v| v.as_array())
                .unwrap();
            assert_eq!(
                entries[0].get("command").and_then(|v| v.as_str()).unwrap(),
                expected_cmd()
            );
            // No matcher set — Kiro treats this as match-all.
            assert!(
                entries[0].get("matcher").is_none(),
                "matcher should be omitted; preset filters tools internally"
            );
        }
    }

    #[test]
    fn s1b_fresh_install_does_not_overwrite_existing_name() {
        // If the user has already configured an agent file with a
        // custom name, our installer must NOT clobber it.
        let (_td, path) = setup_test_env();
        fs::write(&path, r#"{"name": "my-custom-agent"}"#).unwrap();

        KiroInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let cfg = read_agent(&path);
        assert_eq!(
            cfg.get("name").and_then(|v| v.as_str()),
            Some("my-custom-agent"),
            "user-set name must be preserved"
        );
    }

    #[test]
    fn s2_idempotent_already_installed() {
        let (_td, path) = setup_test_env();
        KiroInstaller::install_hooks_at(&path, &params(), false).unwrap();
        let diff2 = KiroInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff2.is_none(), "second install should be a no-op");
    }

    #[test]
    fn s3_preserves_unrelated_agent_settings_and_hooks() {
        let (_td, path) = setup_test_env();
        let unrelated = r#"{
  "name": "default",
  "model": "claude-sonnet-4-5",
  "tools": ["fs_read", "fs_write"],
  "hooks": {
    "preToolUse": [
      {"matcher": "execute_bash", "command": "echo unrelated"}
    ]
  }
}"#;
        fs::write(&path, unrelated).unwrap();

        KiroInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // Original settings preserved.
        assert!(content.contains("\"name\": \"default\""), "{content}");
        assert!(content.contains("claude-sonnet-4-5"), "{content}");
        assert!(content.contains("\"tools\""), "{content}");
        assert!(content.contains("echo unrelated"), "{content}");

        let cfg = read_agent(&path);
        assert_eq!(count_kiro_entries(&cfg, "preToolUse"), 1);
        assert_eq!(count_kiro_entries(&cfg, "postToolUse"), 1);
    }

    #[test]
    fn s4_updates_outdated_command_path() {
        let (_td, path) = setup_test_env();
        let stale = r#"{
  "hooks": {
    "preToolUse": [{"command": "/old/path/git-ai checkpoint kiro --hook-input stdin"}],
    "postToolUse": [{"command": "/old/path/git-ai checkpoint kiro --hook-input stdin"}]
  }
}"#;
        fs::write(&path, stale).unwrap();

        let diff = KiroInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "stale path should produce a diff");

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("/usr/local/bin/git-ai"), "{content}");
        assert!(!content.contains("/old/path/git-ai"), "{content}");

        let cfg = read_agent(&path);
        for event in &KIRO_HOOK_EVENTS {
            assert_eq!(count_kiro_entries(&cfg, event), 1);
        }
    }

    #[test]
    fn s5_dedups_existing_kiro_entries_for_same_event() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let dup = format!(
            r#"{{
  "hooks": {{
    "preToolUse": [
      {{"command": "{cmd}"}},
      {{"command": "{cmd}"}}
    ]
  }}
}}"#
        );
        fs::write(&path, dup).unwrap();

        KiroInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let cfg = read_agent(&path);
        assert_eq!(count_kiro_entries(&cfg, "preToolUse"), 1);
    }

    #[test]
    fn s6_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = KiroInstaller::install_hooks_at(&path, &params(), true).unwrap();
        assert!(diff.is_some(), "dry run still computes a diff");
        assert!(!path.exists(), "dry run must not write the file");
    }

    #[test]
    fn s7_create_dir_on_first_install() {
        let td = TempDir::new().unwrap();
        let nested = td
            .path()
            .join("foo")
            .join(".kiro")
            .join("agents")
            .join("default.json");
        assert!(!nested.parent().unwrap().exists());
        KiroInstaller::install_hooks_at(&nested, &params(), false).unwrap();
        assert!(nested.exists());
    }

    // ---- Uninstall ----

    #[test]
    fn u1_uninstall_removes_only_kiro_entries() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let mixed = format!(
            r#"{{
  "name": "default",
  "hooks": {{
    "preToolUse": [
      {{"command": "{cmd}"}},
      {{"matcher": "execute_bash", "command": "echo not ours"}}
    ]
  }}
}}"#
        );
        fs::write(&path, mixed).unwrap();

        let diff = KiroInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_some());

        let cfg = read_agent(&path);
        assert_eq!(count_kiro_entries(&cfg, "preToolUse"), 0);
        // User-defined hook + agent name preserved.
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo not ours"), "{content}");
        assert!(content.contains("\"name\": \"default\""), "{content}");
    }

    #[test]
    fn u2_uninstall_returns_none_when_no_kiro_entries() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            r#"{"hooks": {"preToolUse": [{"command": "echo unrelated"}]}}"#,
        )
        .unwrap();
        let diff = KiroInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u3_uninstall_returns_none_when_agent_missing() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();
        let diff = KiroInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u4_uninstall_removes_emptied_event_keys_and_hooks_block() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let only_ours = format!(
            r#"{{
  "name": "default",
  "hooks": {{
    "preToolUse": [{{"command": "{cmd}"}}],
    "postToolUse": [{{"command": "{cmd}"}}]
  }}
}}"#
        );
        fs::write(&path, only_ours).unwrap();

        KiroInstaller::uninstall_hooks_at(&path, false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("preToolUse"),
            "empty preToolUse key should be removed; got: {content}"
        );
        assert!(
            !content.contains("\"hooks\""),
            "empty hooks block should be removed; got: {content}"
        );
        // Agent name preserved.
        assert!(content.contains("\"name\": \"default\""), "{content}");
    }

    #[test]
    fn u5_dry_run_does_not_write() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let initial = format!(r#"{{"hooks": {{"preToolUse": [{{"command": "{cmd}"}}]}}}}"#);
        fs::write(&path, &initial).unwrap();

        let diff = KiroInstaller::uninstall_hooks_at(&path, true).unwrap();
        assert!(diff.is_some());
        assert_eq!(fs::read_to_string(&path).unwrap(), initial);
    }

    // ---- Error handling ----

    #[test]
    fn e1_invalid_json_install_errors() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "{not json}").unwrap();
        let result = KiroInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Failed to parse Kiro agent JSON"), "{msg}");
    }

    #[test]
    fn e2_invalid_json_uninstall_returns_none() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "[bad].").unwrap();
        let diff = KiroInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn e3_root_must_be_object() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "[]").unwrap();
        let result = KiroInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
    }

    #[test]
    fn e4_hooks_must_be_object() {
        let (_td, path) = setup_test_env();
        fs::write(&path, r#"{"hooks": []}"#).unwrap();
        let result = KiroInstaller::install_hooks_at(&path, &params(), false);
        assert!(result.is_err());
    }

    // ---- hook_status ----

    #[test]
    fn check_status_reports_installed_when_present() {
        let cmd = expected_cmd();
        let v: Value = serde_json::from_str(&format!(
            r#"{{"hooks": {{
                "preToolUse": [{{"command": "{cmd}"}}],
                "postToolUse": [{{"command": "{cmd}"}}]
            }}}}"#
        ))
        .unwrap();
        let (installed, up_to_date) = KiroInstaller::hook_status(&v, &cmd);
        assert!(installed);
        assert!(up_to_date);
    }
}
