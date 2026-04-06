use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const DROID_PRE_TOOL_CMD: &str = "checkpoint droid --hook-input stdin";
const DROID_POST_TOOL_CMD: &str = "checkpoint droid --hook-input stdin";
const DROID_CATCH_ALL_MATCHER: &str = "*";

pub struct DroidInstaller;

impl DroidInstaller {
    fn settings_path() -> PathBuf {
        home_dir().join(".factory").join("settings.json")
    }

    /// Returns `(hooks_installed, hooks_up_to_date)` from a parsed settings value.
    /// `hooks_installed` = git-ai checkpoint command exists in ANY matcher block.
    /// `hooks_up_to_date` = git-ai checkpoint command exists in the `"*"` catch-all block.
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
                .map(|m| m == DROID_CATCH_ALL_MATCHER)
                .unwrap_or(false);

            let has_git_ai = block
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .map(is_git_ai_checkpoint_command)
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

        let binary_path = params.binary_path.to_string_lossy().to_string();
        let pre_tool_cmd = format!("{} {}", binary_path, DROID_PRE_TOOL_CMD);
        let post_tool_cmd = format!("{} {}", binary_path, DROID_POST_TOOL_CMD);

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

            // Step 1: Strip git-ai from every non-catch-all matcher block (migration).
            // Track which blocks we emptied so we can remove them below.
            let mut emptied_by_migration = vec![false; hook_type_array.len()];
            for (i, block) in hook_type_array.iter_mut().enumerate() {
                let is_catch_all = block
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .map(|m| m == DROID_CATCH_ALL_MATCHER)
                    .unwrap_or(false);
                if !is_catch_all
                    && let Some(hooks) = block.get_mut("hooks").and_then(|h| h.as_array_mut())
                {
                    let before = hooks.len();
                    hooks.retain(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .map(|cmd| !is_git_ai_checkpoint_command(cmd))
                            .unwrap_or(true)
                    });
                    if hooks.is_empty() && before > 0 {
                        emptied_by_migration[i] = true;
                    }
                }
            }
            // Remove blocks that we emptied; leave pre-existing empty blocks alone.
            let mut i = 0;
            hook_type_array.retain(|_| {
                let remove = emptied_by_migration[i];
                i += 1;
                !remove
            });

            // Step 2: Find or create the "*" catch-all matcher block.
            let catch_all_idx = hook_type_array
                .iter()
                .position(|b| {
                    b.get("matcher")
                        .and_then(|m| m.as_str())
                        .map(|m| m == DROID_CATCH_ALL_MATCHER)
                        .unwrap_or(false)
                })
                .unwrap_or_else(|| {
                    hook_type_array.push(json!({
                        "matcher": DROID_CATCH_ALL_MATCHER,
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
                    && is_git_ai_checkpoint_command(cmd)
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
                            let is_dup = is_git_ai_checkpoint_command(cmd);
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

        // Add claudeHooksImported flag if it doesn't exist.
        if let Some(hooks) = merged.get_mut("hooks").and_then(|h| h.as_object_mut())
            && !hooks.contains_key("claudeHooksImported")
        {
            hooks.insert("claudeHooksImported".to_string(), json!(true));
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
                                !is_git_ai_checkpoint_command(cmd)
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

impl HookInstaller for DroidInstaller {
    fn name(&self) -> &str {
        "Droid"
    }

    fn id(&self) -> &str {
        "droid"
    }

    fn process_names(&self) -> Vec<&str> {
        vec!["droid"]
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let has_dotfiles = home_dir().join(".factory").exists();

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
        let settings_path = temp_dir.path().join(".factory").join("settings.json");
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
        format!("{} {}", binary_path().display(), DROID_PRE_TOOL_CMD)
    }

    fn read_settings(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn hooks_in_catch_all<'a>(settings: &'a Value, hook_type: &str) -> Vec<&'a Value> {
        let Some(blocks) = settings
            .get("hooks")
            .and_then(|h| h.get(hook_type))
            .and_then(|v| v.as_array())
        else {
            return Vec::new();
        };
        blocks
            .iter()
            .find(|b| {
                b.get("matcher")
                    .and_then(|m| m.as_str())
                    .map(|m| m == DROID_CATCH_ALL_MATCHER)
                    .unwrap_or(false)
            })
            .and_then(|b| b.get("hooks").and_then(|h| h.as_array()))
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    // ---- Install scenarios ----

    #[test]
    fn s1_fresh_install_creates_catch_all_block() {
        let (_td, path) = setup_test_env();
        fs::remove_file(&path).ok();

        let diff = DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some());

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let hooks = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(hooks.len(), 1, "{hook_type}: expected 1 hook in catch-all");
            assert_eq!(
                hooks[0].get("command").and_then(|c| c.as_str()).unwrap(),
                expected_cmd()
            );
        }
        // claudeHooksImported flag should be set
        assert_eq!(
            settings
                .get("hooks")
                .and_then(|h| h.get("claudeHooksImported")),
            Some(&json!(true))
        );
    }

    #[test]
    fn s2_idempotent_already_on_catch_all() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": cmd}]}],
                    "PostToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": cmd}]}],
                    "claudeHooksImported": true
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let diff = DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_none(), "should be idempotent");
    }

    #[test]
    fn s3_migration_old_matcher_no_user_hooks() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": cmd}]}],
                    "PostToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": cmd}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let hooks = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(hooks.len(), 1, "{hook_type}: git-ai should be in catch-all");

            // The old matcher block had only our hook, so it must be removed entirely.
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
                    "PreToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [
                        {"type":"command","command": "echo before"},
                        {"type":"command","command": cmd}
                    ]}],
                    "PostToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [
                        {"type":"command","command": "prettier --write"},
                        {"type":"command","command": cmd}
                    ]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();

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
                .find(|b| {
                    b.get("matcher").and_then(|m| m.as_str())
                        == Some("^(Edit|Write|Create|ApplyPatch)$")
                })
                .unwrap();
            let old_hooks = old_block.get("hooks").and_then(|h| h.as_array()).unwrap();
            assert!(
                old_hooks
                    .iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(*user_cmd))
            );
            assert!(!old_hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_git_ai_checkpoint_command)
                    .unwrap_or(false)
            }));
        }
    }

    #[test]
    fn s5_fresh_install_user_has_old_matcher_hook() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": "echo user"}]}],
                    "PostToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": "echo user"}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(catch_all.len(), 1);

            let blocks = settings
                .get("hooks")
                .and_then(|h| h.get(*hook_type))
                .and_then(|v| v.as_array())
                .unwrap();
            let old_block = blocks
                .iter()
                .find(|b| {
                    b.get("matcher").and_then(|m| m.as_str())
                        == Some("^(Edit|Write|Create|ApplyPatch)$")
                })
                .unwrap();
            let old_hooks = old_block.get("hooks").and_then(|h| h.as_array()).unwrap();
            assert_eq!(old_hooks.len(), 1);
            assert_eq!(
                old_hooks[0]
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap(),
                "echo user"
            );
        }
    }

    #[test]
    fn s6_fresh_install_user_has_catch_all_hook() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": "audit-tool"}]}],
                    "PostToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": "audit-tool"}]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(catch_all.len(), 2);
            assert_eq!(
                catch_all[0]
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap(),
                "audit-tool"
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
    fn s7_idempotent_user_catch_all_plus_git_ai() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let before = json!({
            "hooks": {
                "PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": "audit-tool"}, {"type":"command","command": cmd}]}],
                "PostToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": "audit-tool"}, {"type":"command","command": cmd}]}],
                "claudeHooksImported": true
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&before).unwrap()).unwrap();
        let diff = DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn s8_deduplication_git_ai_in_both_blocks() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "*", "hooks": [{"type":"command","command": cmd}]},
                        {"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": "user"}, {"type":"command","command": cmd}]}
                    ],
                    "PostToolUse": [
                        {"matcher": "*", "hooks": [{"type":"command","command": cmd}]},
                        {"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": "user"}, {"type":"command","command": cmd}]}
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(catch_all.len(), 1);

            let blocks = settings
                .get("hooks")
                .and_then(|h| h.get(*hook_type))
                .and_then(|v| v.as_array())
                .unwrap();
            let old_block = blocks
                .iter()
                .find(|b| {
                    b.get("matcher").and_then(|m| m.as_str())
                        == Some("^(Edit|Write|Create|ApplyPatch)$")
                })
                .unwrap();
            let old_hooks = old_block.get("hooks").and_then(|h| h.as_array()).unwrap();
            assert!(
                old_hooks
                    .iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some("user"))
            );
            assert!(!old_hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_git_ai_checkpoint_command)
                    .unwrap_or(false)
            }));
        }
    }

    #[test]
    fn s9_deduplication_two_git_ai_in_catch_all() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": cmd}, {"type":"command","command": cmd}]}],
                    "PostToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": cmd}, {"type":"command","command": cmd}]}],
                    "claudeHooksImported": true
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert_eq!(catch_all.len(), 1);
        }
    }

    #[test]
    fn s10_stale_command_upgraded() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": "/old/git-ai checkpoint droid"}]}],
                    "PostToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": "/old/git-ai checkpoint droid"}]}],
                    "claudeHooksImported": true
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::install_hooks_at(&path, &params(), false).unwrap();

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

    // ---- Uninstall scenarios ----

    #[test]
    fn u1_uninstall_from_catch_all() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": cmd}]}],
                    "PostToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": cmd}]}],
                    "claudeHooksImported": true
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let diff = DroidInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_some());

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let catch_all = hooks_in_catch_all(&settings, hook_type);
            assert!(!catch_all.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_git_ai_checkpoint_command)
                    .unwrap_or(false)
            }));
        }
    }

    #[test]
    fn u2_uninstall_from_old_matcher_preserves_user_hook() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [
                        {"type":"command","command": "echo before"},
                        {"type":"command","command": cmd}
                    ]}],
                    "PostToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [
                        {"type":"command","command": "echo before"},
                        {"type":"command","command": cmd}
                    ]}]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::uninstall_hooks_at(&path, false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let blocks = settings
                .get("hooks")
                .and_then(|h| h.get(*hook_type))
                .and_then(|v| v.as_array())
                .unwrap();
            let old_block = blocks
                .iter()
                .find(|b| {
                    b.get("matcher").and_then(|m| m.as_str())
                        == Some("^(Edit|Write|Create|ApplyPatch)$")
                })
                .unwrap();
            let hooks = old_block.get("hooks").and_then(|h| h.as_array()).unwrap();
            assert!(
                hooks
                    .iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some("echo before"))
            );
            assert!(!hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_git_ai_checkpoint_command)
                    .unwrap_or(false)
            }));
        }
    }

    #[test]
    fn u3_uninstall_from_multiple_blocks() {
        let (_td, path) = setup_test_env();
        let cmd = expected_cmd();
        let user = "echo user";
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "*", "hooks": [{"type":"command","command": cmd}, {"type":"command","command": user}]},
                        {"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": cmd}]}
                    ],
                    "PostToolUse": [
                        {"matcher": "*", "hooks": [{"type":"command","command": cmd}]},
                        {"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": cmd}, {"type":"command","command": user}]}
                    ],
                    "claudeHooksImported": true
                }
            }))
            .unwrap(),
        )
        .unwrap();

        DroidInstaller::uninstall_hooks_at(&path, false).unwrap();

        let settings = read_settings(&path);
        for hook_type in &["PreToolUse", "PostToolUse"] {
            let all_blocks = settings
                .get("hooks")
                .and_then(|h| h.get(*hook_type))
                .and_then(|v| v.as_array())
                .unwrap();
            for block in all_blocks {
                let empty_hooks: Vec<Value> = Vec::new();
                let hooks = block
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .unwrap_or(&empty_hooks);
                assert!(!hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(is_git_ai_checkpoint_command)
                        .unwrap_or(false)
                }));
            }
        }
    }

    #[test]
    fn u4_noop_uninstall_when_no_git_ai() {
        let (_td, path) = setup_test_env();
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({"hooks": {"PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": "echo hello"}]}]}}))
                .unwrap(),
        )
        .unwrap();

        let diff = DroidInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_none());
    }

    // ---- check_hooks scenarios ----

    #[test]
    fn c1_no_hooks_returns_not_installed() {
        let (installed, up_to_date) = DroidInstaller::hook_status(&json!({}));
        assert!(!installed);
        assert!(!up_to_date);
    }

    #[test]
    fn c2_git_ai_in_catch_all_returns_up_to_date() {
        let cmd = expected_cmd();
        let settings = json!({"hooks": {"PreToolUse": [{"matcher": "*", "hooks": [{"type":"command","command": cmd}]}]}});
        let (installed, up_to_date) = DroidInstaller::hook_status(&settings);
        assert!(installed);
        assert!(up_to_date);
    }

    #[test]
    fn c3_git_ai_only_in_old_matcher_not_up_to_date() {
        let cmd = expected_cmd();
        let settings = json!({"hooks": {"PreToolUse": [{"matcher": "^(Edit|Write|Create|ApplyPatch)$", "hooks": [{"type":"command","command": cmd}]}]}});
        let (installed, up_to_date) = DroidInstaller::hook_status(&settings);
        assert!(installed);
        assert!(!up_to_date);
    }
}
