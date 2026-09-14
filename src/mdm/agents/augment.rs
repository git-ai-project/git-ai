//! Hook installer for Augment Code (Auggie CLI).
//!
//! Hooks are configured in `~/.augment/settings.json`, schema mirroring
//! Claude Code's, per <https://docs.augmentcode.com/cli/config> and
//! <https://docs.augmentcode.com/cli/hooks>:
//!
//! ```json
//! {"hooks": {"PreToolUse": [{"matcher": ".*",
//!   "hooks": [{"type": "command",
//!              "command": "/path/to/git-ai checkpoint augment --hook-input stdin"}]}]}}
//! ```
//!
//! One entry is installed under the `".*"` catch-all matcher for each of
//! `PreToolUse`/`PostToolUse`; the preset itself filters to the tools it
//! actually checkpoints. Idempotent: re-running leaves the config
//! unchanged when already current, and only entries this installer owns
//! (matched via `is_git_ai_augment_command`) are touched on uninstall, so
//! user-defined hooks always survive.

use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{
    binary_exists, generate_diff, home_dir, normalize_windows_path_for_shell, parse_jsonc_settings,
    write_atomic,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const AUGMENT_CHECKPOINT_CMD: &str = "checkpoint augment --hook-input stdin";
const AUGMENT_HOOK_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];
const AUGMENT_CATCH_ALL_MATCHER: &str = ".*";

/// Executable names for every supported Augment Code CLI generation: v1
/// (`auggie`) and v2 (`auggie-v2`, and its `cosmos-agent` kernel binary).
/// `process_names` and binary detection both derive from this single list
/// so they can never drift apart.
const AUGMENT_PROCESS_NAMES: [&str; 3] = ["auggie", "auggie-v2", "cosmos-agent"];

/// True if any supported Augment CLI executable name resolves via `exists`.
/// Takes an injectable existence-check function so this is unit-testable
/// without mutating the real process PATH.
fn any_supported_binary_exists(exists: impl Fn(&str) -> bool) -> bool {
    AUGMENT_PROCESS_NAMES.iter().any(|name| exists(name))
}

/// Splits a shell-like command line into words, minimally understanding
/// single- and double-quoted segments and backslash escapes -- just
/// enough to recover the executable and argument words from a command
/// this installer (or an equivalent one) wrote via `shell_quote_path`.
/// This is not a full shell grammar; it is only relied on to identify
/// git-ai-owned hook entries, never to execute anything.
fn split_shell_words(cmd: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                for c2 in chars.by_ref() {
                    if c2 == '\'' {
                        break;
                    }
                    current.push(c2);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c2) = chars.next() {
                    if c2 == '"' {
                        break;
                    }
                    if c2 == '\\'
                        && let Some(&next) = chars.peek()
                        && (next == '"' || next == '\\')
                    {
                        current.push(chars.next().unwrap());
                        continue;
                    }
                    current.push(c2);
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            _ => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

/// True only for git-ai hooks belonging to *this* preset
/// (`<git-ai binary> checkpoint augment ...`).
///
/// Ownership is decided structurally rather than by substring, mirroring
/// how `CodexInstaller::is_git_ai_codex_notify_args` identifies its own
/// entries once a command line is split into words: the first word (the
/// executable) must resolve to a `git-ai`/`git-ai.exe` binary (a bare
/// name or any path ending in one), and the next two words must be
/// exactly `checkpoint` then `augment`. A command like
/// `echo git-ai checkpoint augment` (executable `echo`) or
/// `git-ai checkpoint claude` (wrong preset) is correctly rejected.
fn is_git_ai_augment_command(cmd: &str) -> bool {
    let words = split_shell_words(cmd);
    let Some(bin) = words.first() else {
        return false;
    };
    let has_git_ai_bin = bin == "git-ai"
        || bin.ends_with("/git-ai")
        || bin.ends_with("\\git-ai")
        || bin.ends_with("/git-ai.exe")
        || bin.ends_with("\\git-ai.exe");
    if !has_git_ai_bin {
        return false;
    }
    words.get(1).map(String::as_str) == Some("checkpoint")
        && words.get(2).map(String::as_str) == Some("augment")
}

pub struct AugmentInstaller;

impl AugmentInstaller {
    fn config_dir() -> PathBuf {
        home_dir().join(".augment")
    }

    fn settings_path() -> PathBuf {
        Self::config_dir().join("settings.json")
    }

    /// Quote a shell path if it contains characters a POSIX-style shell
    /// would otherwise split/reinterpret. Mirrors
    /// `GitHubCopilotInstaller::shell_quote_path`.
    fn shell_quote_path(path: &str) -> String {
        if path
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./:=@".contains(character))
        {
            path.to_string()
        } else {
            format!("'{}'", path.replace('\'', "'\\''"))
        }
    }

    fn desired_command(binary_path: &Path) -> String {
        let normalized = normalize_windows_path_for_shell(binary_path);
        let quoted = Self::shell_quote_path(&normalized);
        format!("{quoted} {AUGMENT_CHECKPOINT_CMD}")
    }

    /// Returns `(hooks_installed, hooks_up_to_date)`.
    /// `hooks_installed` = a git-ai-augment entry exists anywhere for at
    ///                      least one event (any matcher block, not just
    ///                      the canonical one).
    /// `hooks_up_to_date` = for every event we install, the *canonical*
    ///                      catch-all block (the first `".*"` matcher
    ///                      block, mirroring `install_hooks_at`'s own
    ///                      selection) holds exactly the current command,
    ///                      AND no owned command also lives outside that
    ///                      block (a legacy matcher, or a duplicate
    ///                      catch-all). Owned commands outside the
    ///                      canonical block mean a reinstall is needed to
    ///                      sweep them up, so they force "needs update"
    ///                      even when the canonical block itself is
    ///                      current.
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

            let canonical_idx = blocks.iter().position(|b| {
                b.get("matcher")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m == AUGMENT_CATCH_ALL_MATCHER)
            });

            let mut canonical_current = false;
            let mut owned_outside_canonical = false;

            for (idx, block) in blocks.iter().enumerate() {
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
                    if Some(idx) == canonical_idx {
                        canonical_current |= cmd == desired_cmd;
                    } else {
                        owned_outside_canonical = true;
                    }
                }
            }

            if canonical_current && !owned_outside_canonical {
                up_to_date_events.push(event);
            }
        }

        let hooks_up_to_date = AUGMENT_HOOK_EVENTS
            .iter()
            .all(|e| up_to_date_events.contains(e));
        (hooks_installed, hooks_up_to_date)
    }

    /// Core of `check_hooks`, with binary-existence and dotfile-presence
    /// injected as plain values rather than read from the real process
    /// PATH / home directory, so the fresh-v2-only-install case (and its
    /// siblings) stay fast, deterministic unit tests with no global-PATH
    /// mutation.
    fn check_hooks_with(
        binary_check: impl Fn(&str) -> bool,
        has_dotfiles: bool,
        settings_path: &Path,
        params: &HookInstallerParams,
    ) -> Result<HookCheckResult, GitAiError> {
        let has_binary = any_supported_binary_exists(binary_check);

        if !has_binary && !has_dotfiles {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        if !settings_path.exists() {
            return Ok(HookCheckResult {
                tool_installed: true,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let content = fs::read_to_string(settings_path)?;
        let existing: Value = parse_jsonc_settings(&content).unwrap_or_else(|_| json!({}));
        let desired_cmd = Self::desired_command(&params.binary_path);
        let (hooks_installed, hooks_up_to_date) = Self::hook_status(&existing, &desired_cmd);

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed,
            hooks_up_to_date,
        })
    }

    fn install_hooks_at(
        settings_path: &Path,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        // Must be a pure no-op filesystem-wise when dry_run: do not create
        // the parent dir here. The real write path (`write_atomic`) already
        // ensures it exists for the non-dry-run case.
        let existing_content = if settings_path.exists() {
            fs::read_to_string(settings_path)?
        } else {
            String::new()
        };

        // Augment documents `settings.json` as JSONC (comments, trailing
        // commas), so parse tolerantly rather than with strict `serde_json`.
        let existing: Value = parse_jsonc_settings(&existing_content).map_err(|e| {
            GitAiError::Generic(format!("Failed to parse Augment settings.json: {e}"))
        })?;
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
            let event_array = hooks_obj
                .get(*event)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // The *canonical* block is the first `".*"` catch-all matcher
            // block, if any, found on the on-disk array (before any
            // mutation below).
            let canonical_pos = event_array.iter().position(|b| {
                b.get("matcher")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m == AUGMENT_CATCH_ALL_MATCHER)
            });

            // Sweep every *other* block (any matcher, including duplicate
            // catch-all blocks) for owned git-ai-augment commands before
            // touching the canonical block. This prevents a hand-written
            // legacy matcher (or a leftover duplicate catch-all) from
            // keeping its own copy of our hook alive after install, which
            // would double-fire the checkpoint and be invisible to
            // `hook_status` (which only tracks the canonical block).
            // Unrelated hooks in a swept block are preserved; a block is
            // dropped only if the sweep leaves it with zero hooks.
            let mut event_array: Vec<Value> = event_array
                .into_iter()
                .enumerate()
                .filter_map(|(idx, mut block)| {
                    if Some(idx) == canonical_pos {
                        return Some(block);
                    }
                    let Some(inner) = block.get("hooks").and_then(|h| h.as_array()) else {
                        return Some(block);
                    };
                    let before = inner.len();
                    let filtered: Vec<Value> = inner
                        .iter()
                        .filter(|hook| {
                            !hook
                                .get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(is_git_ai_augment_command)
                        })
                        .cloned()
                        .collect();
                    if filtered.len() == before {
                        return Some(block);
                    }
                    if filtered.is_empty() {
                        return None;
                    }
                    if let Some(obj) = block.as_object_mut() {
                        obj.insert("hooks".to_string(), Value::Array(filtered));
                    }
                    Some(block)
                })
                .collect();

            // Find (or create) the canonical catch-all matcher block in the
            // swept array.
            let catch_all_idx = event_array
                .iter()
                .position(|b| {
                    b.get("matcher")
                        .and_then(|m| m.as_str())
                        .is_some_and(|m| m == AUGMENT_CATCH_ALL_MATCHER)
                })
                .unwrap_or_else(|| {
                    event_array.push(json!({"matcher": AUGMENT_CATCH_ALL_MATCHER, "hooks": []}));
                    event_array.len() - 1
                });

            // Ensure exactly one git-ai-augment command in that block,
            // deduplicating and refreshing it in place if stale.
            let mut hooks_array = event_array[catch_all_idx]
                .get("hooks")
                .and_then(|h| h.as_array())
                .cloned()
                .unwrap_or_default();

            let existing_idx = hooks_array.iter().position(|hook| {
                hook.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(is_git_ai_augment_command)
            });

            match existing_idx {
                Some(idx) => {
                    hooks_array[idx] = json!({"type": "command", "command": desired_cmd});
                    let mut seen = false;
                    hooks_array.retain(|hook| {
                        let is_ours = hook
                            .get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(is_git_ai_augment_command);
                        if !is_ours {
                            return true;
                        }
                        if seen {
                            false
                        } else {
                            seen = true;
                            true
                        }
                    });
                }
                None => hooks_array.push(json!({"type": "command", "command": desired_cmd})),
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
        let existing: Value = match parse_jsonc_settings(&existing_content) {
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
                        let before = hooks_array.len();
                        hooks_array.retain(|hook| {
                            hook.get("command")
                                .and_then(|c| c.as_str())
                                .map(|cmd| !is_git_ai_augment_command(cmd))
                                .unwrap_or(true)
                        });
                        changed |= hooks_array.len() != before;
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
        AUGMENT_PROCESS_NAMES.to_vec()
    }

    fn check_hooks(&self, params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        Self::check_hooks_with(
            binary_exists,
            Self::config_dir().exists(),
            &Self::settings_path(),
            params,
        )
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

    fn params() -> HookInstallerParams {
        HookInstallerParams {
            binary_path: PathBuf::from("/usr/local/bin/git-ai"),
        }
    }

    fn expected_cmd() -> String {
        AugmentInstaller::desired_command(&params().binary_path)
    }

    fn read_event_blocks(path: &Path, event: &str) -> Value {
        let content = fs::read_to_string(path).unwrap();
        let v: Value = serde_json::from_str(&content).unwrap();
        v["hooks"][event].clone()
    }

    fn count_git_ai_entries(blocks: &Value) -> usize {
        blocks
            .as_array()
            .map(|arr| {
                arr.iter()
                    .flat_map(|b| {
                        b.get("hooks")
                            .and_then(|h| h.as_array())
                            .into_iter()
                            .flatten()
                    })
                    .filter(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(is_git_ai_augment_command)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    // ---- One real end-to-end install/reinstall/uninstall diff assertion ----

    #[test]
    fn install_then_reinstall_then_uninstall_round_trips_cleanly() {
        let (_td, path) = setup_test_env();

        // Fresh install creates the catch-all matcher with our command.
        let diff1 = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff1.is_some());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(&expected_cmd()), "{content}");
        for event in &AUGMENT_HOOK_EVENTS {
            let blocks = read_event_blocks(&path, event);
            assert_eq!(count_git_ai_entries(&blocks), 1);
            assert_eq!(blocks[0]["matcher"], AUGMENT_CATCH_ALL_MATCHER);
        }

        // Reinstall is a no-op.
        assert!(
            AugmentInstaller::install_hooks_at(&path, &params(), false)
                .unwrap()
                .is_none()
        );

        // Uninstall removes only our entries, preserving user hooks added
        // directly to the file.
        let mut settings: Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        settings["hooks"]["PreToolUse"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type": "command", "command": "echo not ours"}));
        fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let diff2 = AugmentInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff2.is_some());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo not ours"), "{content}");
        assert_eq!(
            count_git_ai_entries(&read_event_blocks(&path, "PreToolUse")),
            0
        );

        // A second uninstall is a no-op.
        assert!(
            AugmentInstaller::uninstall_hooks_at(&path, false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn dry_run_never_writes() {
        let td = TempDir::new().unwrap();
        let path = td.path().join(".augment").join("settings.json");
        assert!(!path.parent().unwrap().exists());

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), true).unwrap();
        assert!(diff.is_some(), "dry run still computes a diff");
        assert!(!path.exists(), "dry run must not write the file");
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn creates_parent_dir_on_first_real_install() {
        let td = TempDir::new().unwrap();
        let nested = td
            .path()
            .join("custom")
            .join(".augment")
            .join("settings.json");
        AugmentInstaller::install_hooks_at(&nested, &params(), false).unwrap();
        assert!(nested.exists());
    }

    // ---- Error handling (table-driven) ----

    #[test]
    fn invalid_or_wrong_shaped_settings_json_errors_or_no_ops() {
        let (_td, path) = setup_test_env();

        // Truly invalid content (not just JSONC-with-comments) must still
        // error on install and must not clobber the file on disk.
        fs::write(&path, "{not valid json}").unwrap();
        let err = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap_err();
        assert!(
            err.to_string()
                .contains("Failed to parse Augment settings.json"),
            "{err}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{not valid json}",
            "a failed install must not modify the file"
        );

        fs::write(&path, "{ not json").unwrap();
        assert!(AugmentInstaller::install_hooks_at(&path, &params(), false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");

        // A top-level JSON array is well-formed JSONC but the wrong shape;
        // it must still be refused rather than clobbered.
        fs::write(&path, "[]").unwrap();
        assert!(AugmentInstaller::install_hooks_at(&path, &params(), false).is_err());

        fs::write(&path, "[not valid].").unwrap();
        assert!(
            AugmentInstaller::uninstall_hooks_at(&path, false)
                .unwrap()
                .is_none()
        );

        // A JSON number `serde_json` cannot represent (overflows to
        // infinity) must abort install with a clear error rather than
        // silently persisting `null` in its place, and must not touch
        // the file. This mirrors the plain-invalid-JSON case above.
        let unrepresentable_number = r#"{"x": 1e400}"#;
        fs::write(&path, unrepresentable_number).unwrap();
        let err = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap_err();
        assert!(
            err.to_string().contains("Unrepresentable JSON number"),
            "{err}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            unrepresentable_number,
            "a failed install must not modify the file"
        );

        // Uninstall on the same content must not write `null` over the
        // user's value either; it no-ops like other malformed settings.
        assert!(
            AugmentInstaller::uninstall_hooks_at(&path, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            unrepresentable_number,
            "a no-op uninstall must not modify the file"
        );
    }

    // ---- hook_status (table-driven) ----

    #[test]
    fn hook_status_matrix() {
        let cmd = expected_cmd();
        let cases: &[(&str, bool, bool)] = &[
            (
                r#"{"hooks": {"PreToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}], "PostToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}]}}"#,
                true,
                true,
            ),
            (
                r#"{"hooks": {"PreToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}]}}"#,
                true,
                false,
            ),
            (r#"{"hooks": {}}"#, false, false),
            // An owned command also lives outside the canonical catch-all
            // block (e.g. a hand-written legacy matcher): PreToolUse must
            // report "needs update" even though its canonical block alone
            // is current, because the outside copy means a reinstall is
            // needed to sweep it up.
            (
                r#"{"hooks": {"PreToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}, {"matcher": "other", "hooks": [{"type": "command", "command": "__CMD__"}]}], "PostToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}]}}"#,
                true,
                false,
            ),
            // A duplicate catch-all block also carries an owned command:
            // only the first `".*"` block is canonical, so the duplicate
            // counts as "outside canonical" too.
            (
                r#"{"hooks": {"PreToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}, {"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}], "PostToolUse": [{"matcher": ".*", "hooks": [{"type": "command", "command": "__CMD__"}]}]}}"#,
                true,
                false,
            ),
        ];
        for (template, expect_installed, expect_up_to_date) in cases {
            let v: Value = serde_json::from_str(&template.replace("__CMD__", &cmd)).unwrap();
            let (installed, up_to_date) = AugmentInstaller::hook_status(&v, &cmd);
            assert_eq!(installed, *expect_installed, "{template}");
            assert_eq!(up_to_date, *expect_up_to_date, "{template}");
        }
    }

    // ---- Legacy/duplicate owned-hook sweep on install (Devin finding) ----

    #[test]
    fn legacy_matcher_owned_command_swept_into_canonical_on_install() {
        let (_td, path) = setup_test_env();
        let settings = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "save-file|launch-process",
                        "hooks": [
                            {"type": "command", "command": "/old/path/git-ai checkpoint augment --hook-input stdin"},
                            {"type": "command", "command": "echo audit"}
                        ]
                    }
                ]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some());

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre_blocks = written["hooks"]["PreToolUse"].as_array().unwrap();

        // Exactly one owned command across the whole event, living in the
        // canonical catch-all block.
        assert_eq!(count_git_ai_entries(&written["hooks"]["PreToolUse"]), 1);
        let canonical = pre_blocks
            .iter()
            .find(|b| b["matcher"] == AUGMENT_CATCH_ALL_MATCHER)
            .expect("canonical catch-all block created");
        assert_eq!(canonical["hooks"][0]["command"], expected_cmd());

        // The legacy matcher block survives (not emptied), keeping only its
        // unrelated hook.
        let legacy = pre_blocks
            .iter()
            .find(|b| b["matcher"] == "save-file|launch-process")
            .expect("unrelated legacy block preserved");
        let legacy_hooks = legacy["hooks"].as_array().unwrap();
        assert_eq!(legacy_hooks.len(), 1);
        assert_eq!(legacy_hooks[0]["command"], "echo audit");

        // hook_status now reports fully up to date for both events.
        let (installed, up_to_date) = AugmentInstaller::hook_status(&written, &expected_cmd());
        assert!(installed);
        assert!(up_to_date);

        // Reinstall is a no-op.
        assert!(
            AugmentInstaller::install_hooks_at(&path, &params(), false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn duplicate_catch_all_blocks_consolidated_on_install() {
        let (_td, path) = setup_test_env();
        let settings = json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": ".*", "hooks": [{"type": "command", "command": "/old/path/git-ai checkpoint augment --hook-input stdin"}]},
                    {"matcher": ".*", "hooks": [{"type": "command", "command": "/another/old/git-ai checkpoint augment --hook-input stdin"}]}
                ]
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some());

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let pre_blocks = written["hooks"]["PreToolUse"].as_array().unwrap();
        let catch_all_blocks: Vec<_> = pre_blocks
            .iter()
            .filter(|b| b["matcher"] == AUGMENT_CATCH_ALL_MATCHER)
            .collect();
        assert_eq!(
            catch_all_blocks.len(),
            1,
            "duplicate catch-all block must be swept away: {pre_blocks:?}"
        );
        assert_eq!(count_git_ai_entries(&written["hooks"]["PreToolUse"]), 1);

        let (installed, up_to_date) = AugmentInstaller::hook_status(&written, &expected_cmd());
        assert!(installed);
        assert!(up_to_date);

        // Second install over the now-consolidated file is a no-op.
        assert!(
            AugmentInstaller::install_hooks_at(&path, &params(), false)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn jsonc_settings_with_legacy_owned_hook_sweeps_via_jsonc_parse_path() {
        // Same sweep behaviour as
        // `legacy_matcher_owned_command_swept_into_canonical_on_install`,
        // but through the JSONC (`//`/`/* */`/trailing-comma) parse path,
        // matching Augment's documented settings-file format.
        let (_td, path) = setup_test_env();
        let jsonc = r#"{
            // hand-written legacy hook block, no catch-all matcher yet
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "save-file|launch-process",
                        "hooks": [
                            { "type": "command", "command": "/old/path/git-ai checkpoint augment --hook-input stdin" }, /* stale owned entry */
                            { "type": "command", "command": "echo audit" }, // unrelated, must survive
                        ],
                    },
                ],
            },
        }"#;
        fs::write(&path, jsonc).unwrap();

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some());

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(count_git_ai_entries(&written["hooks"]["PreToolUse"]), 1);
        let pre_blocks = written["hooks"]["PreToolUse"].as_array().unwrap();
        let legacy = pre_blocks
            .iter()
            .find(|b| b["matcher"] == "save-file|launch-process")
            .expect("legacy block preserved");
        let legacy_hooks = legacy["hooks"].as_array().unwrap();
        assert_eq!(legacy_hooks.len(), 1);
        assert_eq!(legacy_hooks[0]["command"], "echo audit");

        let canonical = pre_blocks
            .iter()
            .find(|b| b["matcher"] == AUGMENT_CATCH_ALL_MATCHER)
            .unwrap();
        assert_eq!(canonical["hooks"][0]["command"], expected_cmd());
    }

    // ---- desired_command quoting / normalization (table-driven) ----

    #[test]
    fn desired_command_quoting_matrix() {
        let cases: &[(&str, &str)] = &[
            (
                "/usr/local/bin/git-ai",
                "/usr/local/bin/git-ai checkpoint augment --hook-input stdin",
            ),
            (
                "/opt/My Apps/git-ai",
                "'/opt/My Apps/git-ai' checkpoint augment --hook-input stdin",
            ),
            (
                r"C:\Program Files\git-ai\git-ai.exe",
                "'C:/Program Files/git-ai/git-ai.exe' checkpoint augment --hook-input stdin",
            ),
            (
                r"C:\Users\bob\.git-ai\git-ai.exe",
                "C:/Users/bob/.git-ai/git-ai.exe checkpoint augment --hook-input stdin",
            ),
        ];
        for (input, expected) in cases {
            let cmd = AugmentInstaller::desired_command(Path::new(input));
            assert_eq!(&cmd, expected, "input={input}");
            // Quoting is stable under re-recognition, so reinstall/upgrade
            // over such a path stays idempotent.
            assert!(is_git_ai_augment_command(&cmd), "cmd={cmd}");
            // Auggie's hook executor routes a command through a shell only
            // if its first token ends in a script extension (.ps1/.cmd/
            // .bat/.sh) or the command contains a shell metacharacter
            // (see https://docs.augmentcode.com/cli/hooks); neither must
            // ever be true for our installer-generated command.
            assert!(
                !cmd.chars().any(|c| "|&;><$`".contains(c)),
                "cmd must contain no shell metacharacters: {cmd}"
            );
            assert!(
                ![".ps1", ".cmd", ".bat", ".sh"]
                    .iter()
                    .any(|ext| input.ends_with(ext)),
                "input={input}"
            );
        }
    }

    // ---- is_git_ai_augment_command ownership predicate (table-driven) ----

    #[test]
    fn is_git_ai_augment_command_ownership_matrix() {
        let positive: &[&str] = &[
            "git-ai checkpoint augment --hook-input stdin",
            "/usr/local/bin/git-ai checkpoint augment --hook-input stdin",
            "'/opt/My Apps/git-ai' checkpoint augment --hook-input stdin",
            "C:/Users/bob/.git-ai/git-ai.exe checkpoint augment --hook-input stdin",
            "'C:/Program Files/git-ai/git-ai.exe' checkpoint augment --hook-input stdin",
        ];
        for cmd in positive {
            assert!(is_git_ai_augment_command(cmd), "cmd={cmd}");
        }

        let negative: &[&str] = &[
            // Substring match on a user hook that merely mentions the
            // tokens must NOT be treated as owned (this was the bug).
            "echo git-ai checkpoint augment",
            // Right binary, wrong preset.
            "git-ai checkpoint claude",
            // A wrapper script whose name contains "git-ai" is not the
            // git-ai binary itself.
            "some-git-ai-wrapper checkpoint augment",
            "echo hello",
            "",
            "git-ai",
            "git-ai checkpoint",
        ];
        for cmd in negative {
            assert!(!is_git_ai_augment_command(cmd), "cmd={cmd}");
        }
    }

    #[test]
    fn install_and_reinstall_idempotent_for_spacey_path() {
        let (_td, path) = setup_test_env();
        let spacey_params = HookInstallerParams {
            binary_path: PathBuf::from("/opt/My Apps/git-ai"),
        };
        assert!(
            AugmentInstaller::install_hooks_at(&path, &spacey_params, false)
                .unwrap()
                .is_some()
        );
        assert!(
            AugmentInstaller::install_hooks_at(&path, &spacey_params, false)
                .unwrap()
                .is_none(),
            "reinstall over a spacey path must be a no-op"
        );
        for event in &AUGMENT_HOOK_EVENTS {
            assert_eq!(count_git_ai_entries(&read_event_blocks(&path, event)), 1);
        }
    }

    // ---- v1/v2 binary detection (table-driven) ----

    #[test]
    fn process_names_matches_supported_binary_detection() {
        let installer = AugmentInstaller;
        assert_eq!(installer.process_names(), AUGMENT_PROCESS_NAMES.to_vec());
    }

    #[test]
    fn any_supported_binary_exists_matrix() {
        for name in AUGMENT_PROCESS_NAMES {
            assert!(any_supported_binary_exists(|n| n == name), "name={name}");
        }
        assert!(!any_supported_binary_exists(|_| false));
    }

    #[test]
    fn check_hooks_with_matrix() {
        let (_td, settings_path) = setup_test_env();
        let missing_settings = settings_path.parent().unwrap().join("nonexistent.json");

        // A fresh v1- or v2-only host has the executable on PATH but no
        // `~/.augment` settings directory yet.
        for binary in AUGMENT_PROCESS_NAMES {
            let result = AugmentInstaller::check_hooks_with(
                |n| n == binary,
                false,
                &missing_settings,
                &params(),
            )
            .unwrap();
            assert!(result.tool_installed, "binary={binary}");
            assert!(!result.hooks_installed);
        }

        // Dotfile fallback: no supported binary resolves, but `~/.augment`
        // already exists from a prior install.
        let result =
            AugmentInstaller::check_hooks_with(|_| false, true, &settings_path, &params()).unwrap();
        assert!(result.tool_installed);
        assert!(!result.hooks_installed);

        // Neither binary nor dotfiles: not installed.
        let result =
            AugmentInstaller::check_hooks_with(|_| false, false, &missing_settings, &params())
                .unwrap();
        assert!(!result.tool_installed);
    }

    // ---- JSONC settings support ----

    /// A settings file using Augment's documented JSONC support (`//` line
    /// comments, `/* */` block comments, trailing commas), with a
    /// pre-existing unrelated top-level key and an unrelated user hook that
    /// must survive install/uninstall untouched.
    const JSONC_SETTINGS_WITH_USER_DATA: &str = r#"{
        // user preference, unrelated to hooks
        "theme": "dark",
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": ".*",
                    "hooks": [
                        { "type": "command", "command": "echo user-hook" }, /* keep me */
                    ],
                },
            ],
        },
    }"#;

    #[test]
    fn jsonc_settings_check_install_check_uninstall_round_trip() {
        let (_td, path) = setup_test_env();
        fs::write(&path, JSONC_SETTINGS_WITH_USER_DATA).unwrap();

        // A docs-valid JSONC file must not error out of check_hooks, and
        // must correctly report "not installed" (not a parse failure).
        let result = AugmentInstaller::check_hooks_with(|_| false, true, &path, &params()).unwrap();
        assert!(!result.hooks_installed);

        // Installing against JSONC content must succeed.
        let diff = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some());

        let settings: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings["theme"], "dark", "user key must survive install");
        let pre_hooks = settings["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .unwrap();
        assert!(
            pre_hooks.iter().any(|h| h["command"] == "echo user-hook"),
            "pre-existing user hook must survive install: {pre_hooks:?}"
        );
        assert!(
            pre_hooks
                .iter()
                .any(|h| h["command"].as_str().is_some_and(is_git_ai_augment_command)),
            "our hook must be installed: {pre_hooks:?}"
        );

        let result = AugmentInstaller::check_hooks_with(|_| false, true, &path, &params()).unwrap();
        assert!(result.hooks_installed);
        assert!(result.hooks_up_to_date);

        // Uninstall must remove only our entry, keeping the user's hook and
        // top-level key intact.
        let diff = AugmentInstaller::uninstall_hooks_at(&path, false).unwrap();
        assert!(diff.is_some());
        let settings: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings["theme"], "dark", "user key must survive uninstall");
        let pre_hooks = settings["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .unwrap();
        assert_eq!(pre_hooks.len(), 1);
        assert_eq!(pre_hooks[0]["command"], "echo user-hook");
    }

    #[test]
    fn install_hooks_reserializes_jsonc_settings_as_json() {
        // Documents accepted, intentional behaviour: JSONC input (comments,
        // trailing commas) parses successfully but is rewritten as plain
        // JSON on install -- comments are not preserved. See "Known
        // limitations" in the PR description.
        let (_td, path) = setup_test_env();
        fs::write(&path, JSONC_SETTINGS_WITH_USER_DATA).unwrap();

        AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("//") && !content.contains("/*"),
            "comments must not survive rewrite: {content}"
        );
        // Rewritten content is still well-formed, strict JSON.
        serde_json::from_str::<Value>(&content).unwrap();
    }

    #[test]
    fn whitespace_only_settings_file_treated_as_empty() {
        let (_td, path) = setup_test_env();
        fs::write(&path, "   \n\t  \n").unwrap();

        let result = AugmentInstaller::check_hooks_with(|_| false, true, &path, &params()).unwrap();
        assert!(!result.hooks_installed);

        let diff = AugmentInstaller::install_hooks_at(&path, &params(), false).unwrap();
        assert!(diff.is_some());
        let settings: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(settings["hooks"]["PreToolUse"].is_array());
    }
}
