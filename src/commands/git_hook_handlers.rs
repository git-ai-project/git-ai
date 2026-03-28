use crate::error::GitAiError;
use crate::git::repository::Repository;
use crate::utils::debug_log;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_KEY_CORE_HOOKS_PATH: &str = "core.hooksPath";
const REPO_HOOK_STATE_FILE: &str = "git_hooks_state.json";
const REPO_HOOK_ENABLEMENT_FILE: &str = "git_hooks_enabled";
const REBASE_HOOK_MASK_STATE_FILE: &str = "rebase_hook_mask_state.json";
const GIT_HOOKS_DIR_NAME: &str = "hooks";

// All core hooks recognised by git.
const CORE_GIT_HOOK_NAMES: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "push-to-checkout",
    "reference-transaction",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ForwardMode {
    RepoLocal,
    GlobalFallback,
    #[default]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct RepoHookState {
    #[serde(default)]
    schema_version: String,
    managed_hooks_path: String,
    original_local_hooks_path: Option<String>,
    #[serde(default)]
    forward_mode: ForwardMode,
    #[serde(default, alias = "previous_hooks_path")]
    forward_hooks_path: Option<String>,
    binary_path: String,
}

pub struct RemoveRepoHooksReport {
    pub changed: bool,
    pub managed_hooks_path: PathBuf,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn is_git_hook_binary_name(name: &str) -> bool {
    CORE_GIT_HOOK_NAMES.contains(&name)
}

/// When the binary is invoked as a git hook (e.g. via symlink), warn the user
/// that git-ai no longer uses git hooks and exit 0 so git operations aren't blocked.
pub fn handle_git_hook_invocation(_hook_name: &str, _hook_args: &[String]) -> i32 {
    eprintln!(
        "[git-ai] warning: git-ai no longer uses git hooks for authorship tracking. \
         Run `git-ai git-hooks remove` in this repository to uninstall leftover hooks."
    );
    0
}

/// Returns true if the repo has the git_hooks_enabled marker file.
pub fn is_repo_hooks_enabled(repo: &Repository) -> bool {
    let path = repo_enablement_path(repo);
    path.exists() || path.symlink_metadata().is_ok()
}

/// Remove repo-level managed hooks, restoring the original core.hooksPath if one existed.
pub fn remove_repo_hooks(
    repo: &Repository,
    dry_run: bool,
) -> Result<RemoveRepoHooksReport, GitAiError> {
    let managed_hooks_dir = managed_git_hooks_dir_for_repo(repo);
    let state_path = repo_state_path(repo);
    let enablement_path = repo_enablement_path(repo);
    let rebase_state_path = rebase_hook_mask_state_path(repo);
    let local_config_path = repo_local_config_path(repo);
    let prior_state = read_repo_hook_state(&state_path)?;

    let current_local_hooks =
        read_hooks_path_from_config(&local_config_path, gix_config::Source::Local)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

    let local_points_to_managed = current_local_hooks
        .as_deref()
        .is_some_and(|path| normalize_path(Path::new(path)) == normalize_path(&managed_hooks_dir));

    let restored_local_hooks = prior_state
        .as_ref()
        .and_then(|state| state.original_local_hooks_path.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| {
            normalize_path(path) != normalize_path(&managed_hooks_dir)
                && !is_disallowed_forward_hooks_path(path, Some(repo), Some(&managed_hooks_dir))
        });

    let mut changed = false;
    if local_points_to_managed {
        if let Some(restored_hooks_path) = restored_local_hooks {
            changed |= set_hooks_path_in_config(
                &local_config_path,
                gix_config::Source::Local,
                &restored_hooks_path.to_string_lossy(),
                dry_run,
            )?;
        } else {
            changed |= unset_hooks_path_in_local_config(repo, dry_run)?;
        }
    }

    if managed_hooks_dir.exists() || managed_hooks_dir.symlink_metadata().is_ok() {
        changed = true;
        if !dry_run {
            remove_hook_entry(&managed_hooks_dir)?;
        }
    }

    changed |= delete_state_file(&state_path, dry_run)?;
    changed |= delete_state_file(&enablement_path, dry_run)?;
    changed |= delete_state_file(&rebase_state_path, dry_run)?;

    Ok(RemoveRepoHooksReport {
        changed,
        managed_hooks_path: managed_hooks_dir,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers (used by remove_repo_hooks)
// ---------------------------------------------------------------------------

fn repo_ai_dir(repo: &Repository) -> PathBuf {
    repo.common_dir().join("ai")
}

fn repo_worktree_ai_dir(repo: &Repository) -> PathBuf {
    repo.path().join("ai")
}

fn repo_local_config_path(repo: &Repository) -> PathBuf {
    repo.common_dir().join("config")
}

fn repo_state_path(repo: &Repository) -> PathBuf {
    repo_ai_dir(repo).join(REPO_HOOK_STATE_FILE)
}

fn repo_enablement_path(repo: &Repository) -> PathBuf {
    repo_ai_dir(repo).join(REPO_HOOK_ENABLEMENT_FILE)
}

fn rebase_hook_mask_state_path(repo: &Repository) -> PathBuf {
    repo_worktree_ai_dir(repo).join(REBASE_HOOK_MASK_STATE_FILE)
}

fn managed_git_hooks_dir_for_repo(repo: &Repository) -> PathBuf {
    repo_ai_dir(repo).join(GIT_HOOKS_DIR_NAME)
}

fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_path_inside_component(path: &Path, component: &str) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(component)
    })
}

fn is_path_inside_any_git_ai_dir(path: &Path) -> bool {
    let mut previous_was_git_dir = false;
    for part in path.components() {
        let part = part.as_os_str().to_string_lossy();
        if previous_was_git_dir && part.eq_ignore_ascii_case("ai") {
            return true;
        }
        previous_was_git_dir = part.eq_ignore_ascii_case(".git");
    }
    false
}

fn is_disallowed_forward_hooks_path(
    path: &Path,
    repo: Option<&Repository>,
    managed_hooks_path: Option<&Path>,
) -> bool {
    if is_path_inside_component(path, ".git-ai") {
        return true;
    }
    if is_path_inside_any_git_ai_dir(path) {
        return true;
    }

    if let Some(repo) = repo {
        let ai_dir = repo_ai_dir(repo);
        if normalize_path(path).starts_with(normalize_path(&ai_dir)) {
            return true;
        }
    }

    if let Some(managed_hooks_path) = managed_hooks_path
        && normalize_path(path) == normalize_path(managed_hooks_path)
    {
        return true;
    }

    // Check if path is the managed hooks dir for the repo
    if let Some(repo) = repo {
        normalize_path(path) == normalize_path(&managed_git_hooks_dir_for_repo(repo))
    } else {
        false
    }
}

fn load_config(
    path: &Path,
    source: gix_config::Source,
) -> Result<gix_config::File<'static>, GitAiError> {
    if path.exists() {
        return gix_config::File::from_path_no_includes(path.to_path_buf(), source)
            .map_err(|e| GitAiError::GixError(e.to_string()));
    }
    Ok(gix_config::File::default())
}

fn write_config(path: &Path, cfg: &gix_config::File<'_>) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = cfg.to_bstring();
    fs::write(path, bytes.as_slice())?;
    Ok(())
}

fn read_hooks_path_from_config(path: &Path, source: gix_config::Source) -> Option<String> {
    load_config(path, source).ok().and_then(|cfg| {
        cfg.string(CONFIG_KEY_CORE_HOOKS_PATH)
            .map(|v| v.to_string())
    })
}

fn set_hooks_path_in_config(
    path: &Path,
    source: gix_config::Source,
    value: &str,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let mut cfg = load_config(path, source)?;
    let current = cfg
        .string(CONFIG_KEY_CORE_HOOKS_PATH)
        .map(|v| v.to_string());
    if current.as_deref() == Some(value) {
        return Ok(false);
    }

    if !dry_run {
        cfg.set_raw_value(&CONFIG_KEY_CORE_HOOKS_PATH, value)
            .map_err(|e| GitAiError::GixError(e.to_string()))?;
        write_config(path, &cfg)?;
    }

    Ok(true)
}

fn unset_hooks_path_in_local_config(repo: &Repository, dry_run: bool) -> Result<bool, GitAiError> {
    let local_config_path = repo_local_config_path(repo);
    if read_hooks_path_from_config(&local_config_path, gix_config::Source::Local).is_none() {
        return Ok(false);
    }

    if !dry_run {
        let mut cfg = load_config(&local_config_path, gix_config::Source::Local)?;
        if let Ok(mut hooks_path_values) = cfg.raw_values_mut_by("core", None, "hooksPath") {
            hooks_path_values.delete_all();
        }
        write_config(&local_config_path, &cfg)?;
    }

    Ok(true)
}

fn read_repo_hook_state(path: &Path) -> Result<Option<RepoHookState>, GitAiError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    match serde_json::from_str::<RepoHookState>(&content) {
        Ok(state) => Ok(Some(state)),
        Err(err) => {
            debug_log(&format!(
                "ignoring invalid repo hook state {}: {}",
                path.display(),
                err
            ));
            Ok(None)
        }
    }
}

fn delete_state_file(path: &Path, dry_run: bool) -> Result<bool, GitAiError> {
    if !path.exists() {
        return Ok(false);
    }
    if !dry_run {
        fs::remove_file(path)?;
    }
    Ok(true)
}

fn remove_hook_entry(hook_path: &Path) -> Result<(), GitAiError> {
    let metadata = hook_path.symlink_metadata()?;
    let file_type = metadata.file_type();

    if file_type.is_dir() && !file_type.is_symlink() {
        fs::remove_dir_all(hook_path)?;
    } else {
        fs::remove_file(hook_path)?;
    }
    Ok(())
}
