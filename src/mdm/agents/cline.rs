//! Hook installer for Cline (cline.bot VS Code extension).
//!
//! Cline's hook system is unusual: hooks are not declared in a config
//! file, but as **extensionless executable scripts** dropped into a
//! global hooks directory per the docs at
//! <https://docs.cline.bot/customization/hooks>:
//!
//! - macOS / Linux: `~/Documents/Cline/Hooks/<EventName>` (executable
//!   shell script, no extension)
//! - Windows: `~/Documents/Cline/Hooks/<EventName>.ps1`
//!
//! The script's filename IS the registration; presence in the directory
//! enables it. Each script receives a JSON payload on stdin and may
//! print a JSON response on stdout.
//!
//! For each of `PreToolUse` and `PostToolUse`, we drop a tiny shell
//! script that pipes stdin straight to `git-ai checkpoint cline
//! --hook-input stdin` and emits an empty JSON object so Cline doesn't
//! interpret our exit as an action.
//!
//! The installer is idempotent: re-running it leaves the script
//! unchanged when the desired contents are already present. Only files
//! whose contents we recognize as ours (matched via a marker line and
//! the embedded `git-ai checkpoint cline` invocation) are touched on
//! uninstall, so user-defined hooks survive even if they share the
//! same event name (we leave such files alone with a warning).

use crate::error::GitAiError;
use crate::mdm::hook_installer::{HookCheckResult, HookInstaller, HookInstallerParams};
use crate::mdm::utils::{generate_diff, home_dir, is_git_ai_checkpoint_command, write_atomic};
use std::fs;
use std::path::{Path, PathBuf};

const CLINE_CHECKPOINT_CMD: &str = "checkpoint cline --hook-input stdin";
const CLINE_HOOK_EVENTS: [&str; 2] = ["PreToolUse", "PostToolUse"];
/// Marker line included in scripts we own so uninstall and dedup can
/// safely recognize them. Avoid editing — uninstall's identification
/// depends on this exact string.
const CLINE_SCRIPT_MARKER: &str = "# git-ai-cline-hook (managed by git-ai install-hooks)";

#[cfg(unix)]
fn render_script(binary_path: &Path) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         {marker}\n\
         {binary} {args} || true\n\
         printf '{{}}\\n'\n",
        marker = CLINE_SCRIPT_MARKER,
        binary = binary_path.display(),
        args = CLINE_CHECKPOINT_CMD,
    )
}

#[cfg(not(unix))]
fn render_script(binary_path: &Path) -> String {
    // PowerShell variant for Windows. The `.ps1` extension is added at
    // the script-path layer, not here.
    format!(
        "{marker}\n\
         $payload = [Console]::In.ReadToEnd()\n\
         $payload | & '{binary}' {args}\n\
         Write-Output '{{}}'\n",
        marker = CLINE_SCRIPT_MARKER,
        binary = binary_path.display(),
        args = CLINE_CHECKPOINT_CMD,
    )
}

#[cfg(unix)]
fn script_path_for(hooks_dir: &Path, event: &str) -> PathBuf {
    hooks_dir.join(event)
}

#[cfg(not(unix))]
fn script_path_for(hooks_dir: &Path, event: &str) -> PathBuf {
    hooks_dir.join(format!("{event}.ps1"))
}

/// Returns true if the file's contents look like a script we own.
/// The check is conservative: requires both the marker comment AND a
/// line that contains `git-ai checkpoint cline ...`. User-defined hook
/// scripts that happen to invoke git-ai for some other reason are NOT
/// matched.
fn is_git_ai_cline_script(content: &str) -> bool {
    if !content.contains(CLINE_SCRIPT_MARKER) {
        return false;
    }
    content.lines().any(|line| {
        let trimmed = line.trim();
        // PowerShell variant uses `& '<path>' checkpoint cline ...`;
        // the shared helper recognizes the inner `git-ai checkpoint cline`
        // substring after stripping the leading `& ' ... '` quoting.
        let stripped = trimmed.trim_start_matches("& ").trim_matches('\'');
        is_git_ai_checkpoint_command(stripped) && stripped.contains("checkpoint cline")
    })
}

pub struct ClineInstaller;

impl ClineInstaller {
    fn hooks_dir() -> PathBuf {
        // Cline reads from `~/Documents/Cline/Hooks/` on all platforms
        // per the docs. On Linux this is the literal path even though
        // the OS doesn't typically have a `Documents` folder by
        // convention; users who run Cline on Linux must follow the
        // docs' placement.
        home_dir().join("Documents").join("Cline").join("Hooks")
    }

    fn install_hooks_at(
        hooks_dir: &Path,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        if !dry_run {
            fs::create_dir_all(hooks_dir)?;
        }

        let script_content = render_script(&params.binary_path);
        let mut all_diffs = String::new();
        let mut any_changed = false;

        for event in &CLINE_HOOK_EVENTS {
            let path = script_path_for(hooks_dir, event);

            let existing_content = if path.exists() {
                fs::read_to_string(&path).unwrap_or_default()
            } else {
                String::new()
            };

            // If a non-git-ai user script already exists at this path,
            // refuse to overwrite. Surface as a warning by skipping.
            if !existing_content.is_empty() && !is_git_ai_cline_script(&existing_content) {
                eprintln!(
                    "warning: skipping Cline {event} hook — a user-defined script already exists at {}",
                    path.display()
                );
                continue;
            }

            if existing_content == script_content {
                continue;
            }

            any_changed = true;
            let diff = generate_diff(&path, &existing_content, &script_content);
            all_diffs.push_str(&diff);

            if !dry_run {
                write_atomic(&path, script_content.as_bytes())?;
                set_executable(&path)?;
            }
        }

        if !any_changed {
            return Ok(None);
        }

        Ok(Some(all_diffs))
    }

    fn uninstall_hooks_at(hooks_dir: &Path, dry_run: bool) -> Result<Option<String>, GitAiError> {
        if !hooks_dir.exists() {
            return Ok(None);
        }

        let mut all_diffs = String::new();
        let mut any_changed = false;

        for event in &CLINE_HOOK_EVENTS {
            let path = script_path_for(hooks_dir, event);
            if !path.exists() {
                continue;
            }
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !is_git_ai_cline_script(&content) {
                continue;
            }
            any_changed = true;
            let diff = generate_diff(&path, &content, "");
            all_diffs.push_str(&diff);

            if !dry_run {
                fs::remove_file(&path)?;
            }
        }

        if !any_changed {
            return Ok(None);
        }

        Ok(Some(all_diffs))
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), GitAiError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    let mode = perms.mode() | 0o755;
    perms.set_mode(mode);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), GitAiError> {
    // Windows inherits ACLs from the parent dir; the .ps1 extension
    // marks it as PowerShell-runnable.
    Ok(())
}

impl HookInstaller for ClineInstaller {
    fn name(&self) -> &str {
        "Cline"
    }

    fn id(&self) -> &str {
        "cline"
    }

    fn process_names(&self) -> Vec<&str> {
        // Cline ships as a VS Code extension (`saoudrizwan.claude-dev`)
        // and does not install a standalone CLI, so there's no binary
        // for `binary_exists` to find. We rely on the Hooks-dir
        // existence check in `check_hooks` instead.
        vec![]
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        let hooks_dir = Self::hooks_dir();
        if !hooks_dir.exists() {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        // Hooks dir exists; consider Cline "installed" for our purposes.
        let mut hooks_installed = false;
        let mut all_present = true;
        for event in &CLINE_HOOK_EVENTS {
            let path = script_path_for(&hooks_dir, event);
            if !path.exists() {
                all_present = false;
                continue;
            }
            let content = fs::read_to_string(&path).unwrap_or_default();
            if is_git_ai_cline_script(&content) {
                hooks_installed = true;
            } else {
                all_present = false;
            }
        }

        Ok(HookCheckResult {
            tool_installed: true,
            hooks_installed,
            hooks_up_to_date: hooks_installed && all_present,
        })
    }

    fn install_hooks(
        &self,
        params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Self::install_hooks_at(&Self::hooks_dir(), params, dry_run)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Self::uninstall_hooks_at(&Self::hooks_dir(), dry_run)
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

    fn setup_test_env() -> (TempDir, PathBuf) {
        let td = TempDir::new().unwrap();
        let hooks_dir = td.path().join("Documents").join("Cline").join("Hooks");
        // Don't pre-create — let install do it.
        (td, hooks_dir)
    }

    // ---- is_git_ai_cline_script ----

    #[test]
    fn test_is_git_ai_cline_script_matches_managed_script() {
        let content = render_script(&binary_path());
        assert!(is_git_ai_cline_script(&content));
    }

    #[test]
    fn test_is_git_ai_cline_script_rejects_unrelated() {
        assert!(!is_git_ai_cline_script("#!/bin/bash\necho hi\n"));
        assert!(!is_git_ai_cline_script(""));
        assert!(!is_git_ai_cline_script(
            "# user-defined script\n/usr/local/bin/git-ai blame foo\n"
        ));
    }

    #[test]
    fn test_is_git_ai_cline_script_rejects_marker_alone() {
        // Marker without the actual git-ai-checkpoint invocation
        // shouldn't count — uninstall should not delete a stranded
        // marker-only file.
        assert!(!is_git_ai_cline_script(&format!(
            "{}\necho only marker\n",
            CLINE_SCRIPT_MARKER
        )));
    }

    #[test]
    fn test_is_git_ai_cline_script_rejects_other_preset() {
        // A script that invokes a sibling preset (e.g. `claude`) must
        // NOT be matched as ours, even if it carries our marker line
        // by accident.
        let content = format!(
            "#!/usr/bin/env bash\n{}\n/usr/local/bin/git-ai checkpoint claude --hook-input stdin\n",
            CLINE_SCRIPT_MARKER
        );
        assert!(!is_git_ai_cline_script(&content));
    }

    // ---- Install ----

    #[test]
    fn s1_fresh_install_creates_pre_and_post_scripts() {
        let (_td, hooks_dir) = setup_test_env();
        let diff = ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();
        assert!(diff.is_some(), "fresh install should produce a diff");

        for event in &CLINE_HOOK_EVENTS {
            let path = script_path_for(&hooks_dir, event);
            assert!(path.exists(), "{path:?} should exist");
            let content = fs::read_to_string(&path).unwrap();
            assert!(is_git_ai_cline_script(&content));
            assert!(content.contains("/usr/local/bin/git-ai"));
            assert!(content.contains("checkpoint cline"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn s1b_fresh_install_sets_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let (_td, hooks_dir) = setup_test_env();
        ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();

        for event in &CLINE_HOOK_EVENTS {
            let path = script_path_for(&hooks_dir, event);
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "{path:?} should be executable");
        }
    }

    #[test]
    fn s2_idempotent_already_installed() {
        let (_td, hooks_dir) = setup_test_env();
        ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();
        let diff2 = ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();
        assert!(diff2.is_none(), "second install should be a no-op");
    }

    #[test]
    fn s3_updates_outdated_binary_path() {
        let (_td, hooks_dir) = setup_test_env();
        let stale_params = HookInstallerParams {
            binary_path: PathBuf::from("/old/path/git-ai"),
        };
        ClineInstaller::install_hooks_at(&hooks_dir, &stale_params, false).unwrap();

        let diff = ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();
        assert!(diff.is_some(), "stale path should produce a diff");

        for event in &CLINE_HOOK_EVENTS {
            let content = fs::read_to_string(script_path_for(&hooks_dir, event)).unwrap();
            assert!(content.contains("/usr/local/bin/git-ai"));
            assert!(!content.contains("/old/path/git-ai"));
        }
    }

    #[test]
    fn s4_dry_run_does_not_write() {
        let (_td, hooks_dir) = setup_test_env();
        let diff = ClineInstaller::install_hooks_at(&hooks_dir, &params(), true).unwrap();
        assert!(diff.is_some(), "dry run still computes a diff");
        for event in &CLINE_HOOK_EVENTS {
            let path = script_path_for(&hooks_dir, event);
            assert!(
                !path.exists(),
                "{path:?} should not be written under dry-run"
            );
        }
        // The hooks dir itself must NOT be created under dry-run, otherwise
        // a subsequent check_hooks would report tool_installed = true purely
        // because the dry-run side-effected the filesystem.
        assert!(
            !hooks_dir.exists(),
            "hooks dir should not be created under dry-run; got: {hooks_dir:?}"
        );
    }

    #[test]
    fn s5_create_hooks_dir_on_first_install() {
        let td = TempDir::new().unwrap();
        let nested = td
            .path()
            .join("foo")
            .join("Documents")
            .join("Cline")
            .join("Hooks");
        assert!(!nested.exists());
        ClineInstaller::install_hooks_at(&nested, &params(), false).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn s6_skips_user_defined_script_at_event_path() {
        // If a user has already dropped a non-git-ai script at the
        // PreToolUse path, install must NOT overwrite it. It should
        // still install the PostToolUse script.
        let (_td, hooks_dir) = setup_test_env();
        fs::create_dir_all(&hooks_dir).unwrap();
        let pre_path = script_path_for(&hooks_dir, "PreToolUse");
        fs::write(&pre_path, "#!/bin/bash\necho user owns this\n").unwrap();

        ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();

        // User script preserved.
        let pre = fs::read_to_string(&pre_path).unwrap();
        assert!(pre.contains("user owns this"));
        assert!(!is_git_ai_cline_script(&pre));

        // Post script installed.
        let post_path = script_path_for(&hooks_dir, "PostToolUse");
        assert!(post_path.exists());
        let post = fs::read_to_string(&post_path).unwrap();
        assert!(is_git_ai_cline_script(&post));
    }

    // ---- Uninstall ----

    #[test]
    fn u1_uninstall_removes_only_managed_scripts() {
        let (_td, hooks_dir) = setup_test_env();
        ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();

        // Drop a user script next to ours.
        let user_path = hooks_dir.join("UserHook");
        fs::write(&user_path, "#!/bin/bash\necho user\n").unwrap();

        ClineInstaller::uninstall_hooks_at(&hooks_dir, false).unwrap();

        for event in &CLINE_HOOK_EVENTS {
            assert!(!script_path_for(&hooks_dir, event).exists());
        }
        // User script untouched.
        assert!(user_path.exists());
        assert!(
            fs::read_to_string(&user_path)
                .unwrap()
                .contains("echo user")
        );
    }

    #[test]
    fn u2_uninstall_returns_none_when_no_managed_scripts() {
        let (_td, hooks_dir) = setup_test_env();
        fs::create_dir_all(&hooks_dir).unwrap();
        // Only user scripts present.
        fs::write(
            script_path_for(&hooks_dir, "PreToolUse"),
            "#!/bin/bash\necho user\n",
        )
        .unwrap();

        let diff = ClineInstaller::uninstall_hooks_at(&hooks_dir, false).unwrap();
        assert!(diff.is_none());
        // User script preserved.
        assert!(script_path_for(&hooks_dir, "PreToolUse").exists());
    }

    #[test]
    fn u3_uninstall_returns_none_when_dir_missing() {
        let td = TempDir::new().unwrap();
        let nope = td.path().join("nonexistent");
        let diff = ClineInstaller::uninstall_hooks_at(&nope, false).unwrap();
        assert!(diff.is_none());
    }

    #[test]
    fn u4_dry_run_does_not_write() {
        let (_td, hooks_dir) = setup_test_env();
        ClineInstaller::install_hooks_at(&hooks_dir, &params(), false).unwrap();

        let diff = ClineInstaller::uninstall_hooks_at(&hooks_dir, true).unwrap();
        assert!(diff.is_some());
        // Files still present after dry-run.
        for event in &CLINE_HOOK_EVENTS {
            assert!(script_path_for(&hooks_dir, event).exists());
        }
    }
}
