//! Hook installer for CodeBuddy CN (Tencent's Claude Code-compatible IDE).
//!
//! CodeBuddy CN's settings.json schema mirrors Claude Code's
//! (`hooks.PreToolUse[].matcher` + `hooks` array of `{type, command}` entries),
//! so the install/uninstall logic mirrors `ClaudeCodeInstaller` closely.
//!
//! Settings location varies by OS (and can be overridden via the
//! `CODEBUDDY_CONFIG_DIR` env var):
//!   - macOS:   `~/Library/Application Support/CodeBuddyExtension/settings.json`
//!   - Linux:   `~/.config/CodeBuddyExtension/settings.json`
//!   - Windows: `%APPDATA%\CodeBuddyExtension\settings.json`
//!
//! Per CodeBuddy CN docs, the `*` catch-all matcher is not supported; tool
//! matchers must be regex patterns. We use `".*"` to match all tools.

use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    generate_diff, home_dir, is_git_ai_checkpoint_command, to_git_bash_path, write_atomic,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const CODEBUDDY_PRE_TOOL_CMD: &str = "checkpoint codebuddy --hook-input stdin";
const CODEBUDDY_POST_TOOL_CMD: &str = "checkpoint codebuddy --hook-input stdin";
/// CodeBuddy CN requires regex matchers; `*` alone does not match. Use `.*`.
const CODEBUDDY_CATCH_ALL_MATCHER: &str = ".*";

/// Returns true only for git-ai commands belonging to *this* preset
/// (`git-ai checkpoint codebuddy ...`). The shared `is_git_ai_checkpoint_command`
/// helper matches any `git-ai checkpoint <preset>` line, which would cause this
/// installer to clobber sibling presets' commands if a user happened to mix them
/// in the same `settings.json`.
fn is_codebuddy_checkpoint_command(cmd: &str) -> bool {
    is_git_ai_checkpoint_command(cmd) && cmd.contains("checkpoint codebuddy")
}

pub struct CodeBuddyInstaller;

impl CodeBuddyInstaller {
    /// CodeBuddy CN settings directory. Honors `CODEBUDDY_CONFIG_DIR` when set,
    /// so users on systems with a non-standard install location can correct
    /// the path without a code change.
    fn settings_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("CODEBUDDY_CONFIG_DIR")
            && !dir.is_empty()
        {
            return PathBuf::from(dir);
        }

        #[cfg(target_os = "macos")]
        {
            home_dir()
                .join("Library")
                .join("Application Support")
                .join("CodeBuddyExtension")
        }

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            home_dir().join(".config").join("CodeBuddyExtension")
        }

        #[cfg(target_os = "windows")]
        {
            std::env::var("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home_dir().join("AppData").join("Roaming"))
                .join("CodeBuddyExtension")
        }
    }

    fn settings_path() -> PathBuf {
        Self::settings_dir().join("settings.json")
    }

    /// Returns `(hooks_installed, hooks_up_to_date)` from a parsed settings value.
    /// `hooks_installed` = git-ai checkpoint command exists in ANY matcher block.
    /// `hooks_up_to_date` = git-ai checkpoint command exists in the catch-all block.
    fn hook_status(settings: &Value) -> (bool, bool) {
        let pre_tool_blocks = settings
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|v| v.as_array());

        let Some(blocks) = pre_tool_blocks else {
            return (false, false);
        };

        let mut hooks_installed = false;
        let mut hooks_up_to_date = false;

        for block in blocks {
            let is_catch_all = block
                .get("matcher")
                .and_then(|m| m.as_str())
                .map(|m| m == CODEBUDDY_CATCH_ALL_MATCHER)
                .unwrap_or(false);

            let has_git_ai = block
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .map(is_codebuddy_checkpoint_command)
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            if has_git_ai {
                hooks_installed = true;
                if is_catch_all {
                    hooks_up_to_date = true;
                }
            }
        }

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
            serde_json::from_str(&existing_content)?
        };

        let binary_path_str = to_git_bash_path(&params.binary_path);
        let pre_tool_cmd = format!("{} {}", binary_path_str, CODEBUDDY_PRE_TOOL_CMD);
        let post_tool_cmd = format!("{} {}", binary_path_str, CODEBUDDY_POST_TOOL_CMD);

        let mut merged = existing.clone();
        let mut hooks_obj = merged.get("hooks").cloned().unwrap_or_else(|| json!({}));

        for (hook_type, desired_cmd) in &[
            ("PreToolUse", &pre_tool_cmd),
            ("PostToolUse", &post_tool_cmd),
        ] {
            let mut hook_type_array = hooks_obj
                .get(*hook_type)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Step 1: Strip git-ai from every non-catch-all matcher block (migration
            // path — preserves user-defined hooks attached to specific tools).
            let mut emptied_by_migration = vec![false; hook_type_array.len()];
            for (i, block) in hook_type_array.iter_mut().enumerate() {
                let is_catch_all = block
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(|m| m == CODEBUDDY_CATCH_ALL_MATCHER)
                    .unwrap_or(false);
                if !is_catch_all
                    && let Some(hooks) = block.get_mut("hooks").and_then(|h| h.as_array_mut())
                {
                    let before = hooks.len();
                    hooks.retain(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .map(|cmd| !is_codebuddy_checkpoint_command(cmd))
                            .unwrap_or(true)
                    });
                    if hooks.is_empty() && before > 0 {
                        emptied_by_migration[i] = true;
                    }
                }
            }
            let mut i = 0;
            hook_type_array.retain(|_| {
                let remove = emptied_by_migration[i];
                i += 1;
                !remove
            });

            // Step 2: Find or create the catch-all matcher block.
            let catch_all_idx = hook_type_array
                .iter()
                .position(|b| {
                    b.get("matcher")
                        .and_then(|m| m.as_str())
                        .map(|m| m == CODEBUDDY_CATCH_ALL_MATCHER)
                        .unwrap_or(false)
                })
                .unwrap_or_else(|| {
                    hook_type_array.push(json!({
                        "matcher": CODEBUDDY_CATCH_ALL_MATCHER,
                        "hooks": []
                    }));
                    hook_type_array.len() - 1
                });

            // Step 3: Ensure exactly one git-ai command in the catch-all block.
            let mut hooks_array = hook_type_array[catch_all_idx]
                .get("hooks")
                .and_then(|h| h.as_array())
                .cloned()
                .unwrap_or_default();

            let mut found_idx: Option<usize> = None;
            let mut needs_update = false;

            for (idx, hook) in hooks_array.iter().enumerate() {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str())
                    && is_codebuddy_checkpoint_command(cmd)
                    && found_idx.is_none()
                {
                    found_idx = Some(idx);
                    if cmd != *desired_cmd {
                        needs_update = true;
                    }
                }
            }

            match found_idx {
                Some(idx) => {
                    if needs_update {
                        hooks_array[idx] = json!({
                            "type": "command",
                            "command": desired_cmd
                        });
                    }
                    let keep_idx = idx;
                    let mut current_idx = 0;
                    hooks_array.retain(|hook| {
                        if current_idx == keep_idx {
                            current_idx += 1;
                            true
                        } else if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                            let is_dup = is_codebuddy_checkpoint_command(cmd);
                            current_idx += 1;
                            !is_dup
                        } else {
                            current_idx += 1;
                            true
                        }
                    });
                }
                None => {
                    hooks_array.push(json!({
                        "type": "command",
                        "command": desired_cmd
                    }));
                }
            }

            if let Some(matcher_block) = hook_type_array[catch_all_idx].as_object_mut() {
                matcher_block.insert("hooks".to_string(), Value::Array(hooks_array));
            }

            if let Some(obj) = hooks_obj.as_object_mut() {
                obj.insert(hook_type.to_string(), Value::Array(hook_type_array));
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
        let existing: Value = serde_json::from_str(&existing_content)?;

        let mut merged = existing.clone();
        let mut hooks_obj = match merged.get("hooks").cloned() {
            Some(h) => h,
            None => return Ok(None),
        };

        let mut changed = false;

        for hook_type in &["PreToolUse", "PostToolUse"] {
            if let Some(hook_type_array) =
                hooks_obj.get_mut(*hook_type).and_then(|v| v.as_array_mut())
            {
                for matcher_block in hook_type_array.iter_mut() {
                    if let Some(hooks_array) = matcher_block
                        .get_mut("hooks")
                        .and_then(|h| h.as_array_mut())
                    {
                        let original_len = hooks_array.len();
                        hooks_array.retain(|hook| {
                            if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                                !is_codebuddy_checkpoint_command(cmd)
                            } else {
                                true
                            }
                        });
                        if hooks_array.len() != original_len {
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

impl HookInstaller for CodeBuddyInstaller {
    fn name(&self) -> &str {
        "CodeBuddy CN"
    }

    fn id(&self) -> &str {
        "codebuddy"
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_dotfiles = Self::settings_dir().exists();

        if !has_dotfiles {
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
        let (hooks_installed, hooks_up_to_date) = Self::hook_status(&existing);

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
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir
            .path()
            .join("CodeBuddyExtension")
            .join("settings.json");
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        (temp_dir, settings_path)
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
        format!("{} {}", binary_path().display(), CODEBUDDY_PRE_TOOL_CMD)
    }

    fn read_settings(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn catch_all_block(hook_type_array: &[Value]) -> Option<&Value> {
        hook_type_array.iter().find(|b| {
            b.get("matcher")
                .and_then(|m| m.as_str())
                .map(|m| m == CODEBUDDY_CATCH_ALL_MATCHER)
                .unwrap_or(false)
        })
    }

    fn hooks_in_catch_all<'a>(settings: &'a Value, hook_type: &str) -> Vec<&'a Value> {
        let Some(blocks) = settings
            .get("hooks")
            .and_then(|h| h.get(hook_type))
            .and_then(|v| v.as_array())
        else {
            return Vec::new();
        };
        catch_all_block(blocks)
            .and_then(|b| b.get("hooks").and_then(|h| h.as_array()))
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    // ---- Install scenarios ----

    #[test]
    fn s1_fresh_install_creates_catch_all_block() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some(), "should produce a diff");

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let hooks = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(hooks.len(), 1, "{hook_type}: expected 1 hook in catch-all");
            assert_eq!(
                hooks[0].get("command").and_then(|c| c.as_str()).unwrap(),
                expected_cmd()
            );
            assert_eq!(
                hooks[0].get("type").and_then(|t| t.as_str()).unwrap(),
                "command"
            );
        }
    }

    #[test]
    fn s2_idempotent_already_on_catch_all() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": cmd}]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": cmd}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let diff = CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_none(), "should return None when already up-to-date");
    }

    #[test]
    fn s3_migration_old_matcher_no_user_hooks() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "Write|Edit|MultiEdit", "hooks": [{"type":"command","command": cmd}]}],
                    "PostToolUse": [{"matcher": "Write|Edit|MultiEdit", "hooks": [{"type":"command","command": cmd}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let hooks = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(hooks.len(), 1, "{hook_type}: expected git-ai in catch-all");

            // Old matcher block (which only had our hook) must be removed.
            let blocks = settings
                .get("hooks")
                .and_then(|h| h.get(*hook_type))
                .and_then(|v| v.as_array())
                .unwrap();
            assert_eq!(
                blocks.len(),
                1,
                "{hook_type}: old matcher block should be removed, only catch-all should remain"
            );
        }
    }

    #[test]
    fn s4_migration_old_matcher_user_hook_preserved() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {"type":"command","command": "echo before"},
                            {"type":"command","command": cmd}
                        ]
                    }],
                    "PostToolUse": [{
                        "matcher": "Write|Edit|MultiEdit",
                        "hooks": [
                            {"type":"command","command": "prettier --write"},
                            {"type":"command","command": cmd}
                        ]
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for (hook_type, user_cmd) in &[
            ("PreToolUse", "echo before"),
            ("PostToolUse", "prettier --write"),
        ] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(catch_all.len(), 1);

            let blocks = settings
                .get("hooks")
                .and_then(|h| h.get(*hook_type))
                .and_then(|v| v.as_array())
                .unwrap();
            let old_block = blocks
                .iter()
                .find(|b| b.get("matcher").and_then(|m| m.as_str()) == Some("Write|Edit|MultiEdit"))
                .expect("old matcher block should still exist");
            let old_hooks = old_block.get("hooks").and_then(|h| h.as_array()).unwrap();
            assert!(
                old_hooks
                    .iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(*user_cmd)),
                "{hook_type}: user hook '{user_cmd}' should still be present"
            );
            assert!(
                !old_hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(is_git_ai_checkpoint_command)
                        .unwrap_or(false)
                }),
                "{hook_type}: git-ai should be removed from old matcher block"
            );
        }
    }

    #[test]
    fn s5_user_catch_all_hook_preserved_alongside_git_ai() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": "my-audit-tool"}]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": "my-audit-tool"}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(
                catch_all.len(),
                2,
                "{hook_type}: should have user hook + git-ai"
            );
            assert_eq!(
                catch_all[0]
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap(),
                "my-audit-tool",
                "user hook should be first"
            );
            assert!(is_git_ai_checkpoint_command(
                catch_all[1]
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap()
            ));
        }
    }

    #[test]
    fn s6_stale_command_upgraded_in_catch_all() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": "/old/path/git-ai checkpoint codebuddy --hook-input stdin"}]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": "/old/path/git-ai checkpoint codebuddy --hook-input stdin"}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(catch_all.len(), 1);
            assert_eq!(
                catch_all[0]
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap(),
                expected_cmd()
            );
        }
    }

    #[test]
    fn s7_dedup_two_git_ai_in_catch_all_block() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": cmd},
                        {"type":"command","command": cmd}
                    ]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": cmd},
                        {"type":"command","command": cmd}
                    ]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(
                catch_all.len(),
                1,
                "{hook_type}: should have exactly 1 after dedup"
            );
        }
    }

    #[test]
    fn s7b_does_not_clobber_sibling_preset_commands() {
        // Regression: CodeBuddy installer must only touch its own
        // `git-ai checkpoint codebuddy ...` lines, never a sibling preset's.
        let (_td, path) = setup_test_env();
        let cb_cmd = expected_cmd();
        let claude_cmd = "/usr/local/bin/git-ai checkpoint claude --hook-input stdin";
        let cursor_cmd = "/usr/local/bin/git-ai checkpoint cursor --hook-input stdin";
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": claude_cmd},
                        {"type":"command","command": cursor_cmd}
                    ]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": claude_cmd}
                    ]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            // Sibling preset commands must still be present, our codebuddy
            // command must be appended (not replace them).
            let cmds: Vec<&str> = catch_all
                .iter()
                .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
                .collect();
            assert!(
                cmds.contains(&claude_cmd),
                "{hook_type}: claude sibling command lost: {:?}",
                cmds
            );
            if *hook_type == "PreToolUse" {
                assert!(
                    cmds.contains(&cursor_cmd),
                    "PreToolUse: cursor sibling command lost: {:?}",
                    cmds
                );
            }
            assert!(
                cmds.iter().any(|c| c == &cb_cmd.as_str()),
                "{hook_type}: codebuddy command not added: {:?}",
                cmds
            );
        }
    }

    #[test]
    fn u_does_not_remove_sibling_preset_commands() {
        // Regression: CodeBuddy uninstall must not remove sibling preset
        // commands that happen to share `git-ai checkpoint` prefix.
        let (_td, path) = setup_test_env();
        let cb_cmd = expected_cmd();
        let claude_cmd = "/usr/local/bin/git-ai checkpoint claude --hook-input stdin";
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": cb_cmd},
                        {"type":"command","command": claude_cmd}
                    ]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": claude_cmd}
                    ]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::uninstall_hooks_at(&path, false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            let cmds: Vec<&str> = catch_all
                .iter()
                .filter_map(|h| h.get("command").and_then(|c| c.as_str()))
                .collect();
            assert!(
                cmds.contains(&claude_cmd),
                "{hook_type}: claude sibling command must survive uninstall: {:?}",
                cmds
            );
            assert!(
                !cmds.iter().any(|c| c == &cb_cmd.as_str()),
                "{hook_type}: codebuddy command should be removed: {:?}",
                cmds
            );
        }
    }

    #[test]
    #[serial_test::serial(codebuddy_env)]
    fn settings_dir_respects_codebuddy_config_dir_env() {
        // SAFETY: env mutation is process-global; serial guard via the env var name itself.
        let dir = "/tmp/codebuddy-test-override-dir";
        // SAFETY: only mutating in test, not used elsewhere concurrently.
        unsafe { std::env::set_var("CODEBUDDY_CONFIG_DIR", dir) };
        let resolved = CodeBuddyInstaller::settings_dir();
        unsafe { std::env::remove_var("CODEBUDDY_CONFIG_DIR") };
        assert_eq!(resolved, PathBuf::from(dir));
    }

    #[test]
    fn s8_creates_missing_parent_dir() {
        let temp_dir = TempDir::new().unwrap();
        let settings_path = temp_dir
            .path()
            .join("missing_dir")
            .join("CodeBuddyExtension")
            .join("settings.json");
        assert!(!settings_path.parent().unwrap().exists());

        let result =
            CodeBuddyInstaller::install_hooks_at(&settings_path, &params(), false).unwrap();

        assert!(result.is_some(), "should report changes for fresh install");
        assert!(settings_path.exists(), "settings.json should be created");
    }

    // ---- Uninstall scenarios ----

    #[test]
    fn u1_uninstall_from_catch_all() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": cmd}]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": cmd}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let diff = CodeBuddyInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_some());

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert!(
                !catch_all.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(is_git_ai_checkpoint_command)
                        .unwrap_or(false)
                }),
                "{hook_type}: git-ai should be removed"
            );
        }
    }

    #[test]
    fn u2_uninstall_preserves_user_hook() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": "my-audit"},
                        {"type":"command","command": cmd}
                    ]}],
                    "PostToolUse": [{"matcher": ".*", "hooks": [
                        {"type":"command","command": "my-audit"},
                        {"type":"command","command": cmd}
                    ]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        CodeBuddyInstaller::uninstall_hooks_at(&path, false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(catch_all.len(), 1);
            assert_eq!(
                catch_all[0]
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap(),
                "my-audit"
            );
        }
    }

    #[test]
    fn u3_noop_uninstall_when_no_git_ai() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": "echo hello"}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let diff = CodeBuddyInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(
            diff.is_none(),
            "should return None when nothing to uninstall"
        );
    }

    #[test]
    fn u4_uninstall_when_settings_missing() {
        let (td, _path) = setup_test_env();
        let missing = td
            .path()
            .join("CodeBuddyExtension")
            .join("nonexistent.json");
        let diff = CodeBuddyInstaller::uninstall_hooks_at(&missing, false).unwrap();
        assert!(diff.is_none());
    }

    // ---- check_hooks scenarios ----

    #[test]
    fn c1_no_hooks_returns_not_installed() {
        let settings = json!({});
        let (installed, up_to_date) = CodeBuddyInstaller::hook_status(&settings);
        assert!(!installed);
        assert!(!up_to_date);
    }

    #[test]
    fn c2_git_ai_in_catch_all_returns_up_to_date() {
        let cmd = expected_cmd();
        let settings = json!({
            "hooks": {
                "PreToolUse": [{"matcher": ".*", "hooks": [{"type":"command","command": cmd}]}]
            }
        });
        let (installed, up_to_date) = CodeBuddyInstaller::hook_status(&settings);
        assert!(installed);
        assert!(up_to_date);
    }

    #[test]
    fn c3_git_ai_only_in_old_matcher_returns_installed_but_not_up_to_date() {
        let cmd = expected_cmd();
        let settings = json!({
            "hooks": {
                "PreToolUse": [{"matcher": "Write|Edit|MultiEdit", "hooks": [{"type":"command","command": cmd}]}]
            }
        });
        let (installed, up_to_date) = CodeBuddyInstaller::hook_status(&settings);
        assert!(installed);
        assert!(!up_to_date);
    }

    #[test]
    #[serial_test::serial(codebuddy_env)]
    fn settings_path_uses_codebuddyextension_dir() {
        // The CODEBUDDY_CONFIG_DIR override would override the default — make
        // sure no concurrent test left it set.
        // SAFETY: env mutation is process-global; serialized via codebuddy_env guard.
        unsafe { std::env::remove_var("CODEBUDDY_CONFIG_DIR") };
        let path = CodeBuddyInstaller::settings_path();
        let s = path.to_string_lossy().to_string();
        assert!(
            s.contains("CodeBuddyExtension"),
            "settings_path should contain CodeBuddyExtension: {}",
            s
        );
        assert!(
            s.ends_with("settings.json"),
            "should end with settings.json: {}",
            s
        );
    }
}
