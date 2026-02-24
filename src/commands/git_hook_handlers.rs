use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::checkout_hooks;
use crate::commands::hooks::commit_hooks;
use crate::commands::hooks::merge_hooks;
use crate::commands::hooks::push_hooks;
use crate::commands::hooks::rebase_hooks;
use crate::commands::hooks::stash_hooks;
use crate::config;
use crate::error::GitAiError;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::{Repository, disable_internal_git_hooks};
use crate::git::sync_authorship::fetch_authorship_notes;
use crate::utils::{debug_log, debug_performance_log_structured};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const CONFIG_KEY_CORE_HOOKS_PATH: &str = "core.hooksPath";
const REPO_HOOK_STATE_FILE: &str = "git_hooks_state.json";
const REPO_HOOK_ENABLEMENT_FILE: &str = "git_hooks_enabled";
const PULL_HOOK_STATE_FILE: &str = "pull_hook_state.json";
const REBASE_HOOK_MASK_STATE_FILE: &str = "rebase_hook_mask_state.json";
const STASH_REF_TX_STATE_FILE: &str = "stash_ref_tx_state.json";
const CHERRY_PICK_BATCH_STATE_FILE: &str = "cherry_pick_batch_state.json";
const GIT_HOOKS_DIR_NAME: &str = "hooks";
const REPO_HOOK_STATE_SCHEMA_VERSION: &str = "repo_hooks/2";
const REBASE_HOOK_MASK_STATE_SCHEMA_VERSION: &str = "rebase_hook_mask/1";
const CHERRY_PICK_BATCH_STATE_SCHEMA_VERSION: &str = "cherry_pick_batch/1";
const REBASE_HOOK_MASK_SUFFIX: &str = ".gitai-masked";

pub const ENV_SKIP_ALL_HOOKS: &str = "GIT_AI_SKIP_ALL_HOOKS";
// Intentionally avoid a GIT_* prefix so git alias shell-command tests don't
// observe extra GIT_* variables in the environment.
pub const ENV_SKIP_MANAGED_HOOKS: &str = "GITAI_SKIP_MANAGED_HOOKS";
const ENV_SKIP_MANAGED_HOOKS_LEGACY: &str = "GIT_AI_SKIP_MANAGED_HOOKS";

// All core hooks recognised by git. Non-managed hooks get managed entries to the git-ai binary
// only when the corresponding hook script exists in the forward target directory, so git-ai can
// properly forward to it at the original path (preserving $0/dirname for Husky-style hooks).
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

// Hooks with managed git-ai behavior. Always installed as managed entries.
// Unix uses symlinks; Windows uses binary copies.
const MANAGED_GIT_HOOK_NAMES: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "post-rewrite",
    "reference-transaction",
];

// During rebases we keep only terminal hooks required for rewrite completion and pull fallback.
const REBASE_TERMINAL_HOOK_NAMES: &[&str] = &["post-rewrite", "post-checkout"];

#[allow(dead_code)]
pub fn core_git_hook_names() -> &'static [&'static str] {
    CORE_GIT_HOOK_NAMES
}

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
    #[serde(default = "repo_hook_state_schema_version")]
    schema_version: String,
    managed_hooks_path: String,
    original_local_hooks_path: Option<String>,
    #[serde(default)]
    forward_mode: ForwardMode,
    #[serde(default, alias = "previous_hooks_path")]
    forward_hooks_path: Option<String>,
    binary_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PullHookState {
    old_head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct RebaseHookMaskState {
    #[serde(default = "rebase_hook_mask_state_schema_version")]
    schema_version: String,
    managed_hooks_path: String,
    masked_hooks: Vec<String>,
    active: bool,
    session_id: String,
    created_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StashReferenceTransactionState {
    before_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct CherryPickBatchMapping {
    source_commit: String,
    new_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct CherryPickBatchState {
    #[serde(default = "cherry_pick_batch_state_schema_version")]
    schema_version: String,
    initial_head: String,
    mappings: Vec<CherryPickBatchMapping>,
    active: bool,
}

#[cfg(unix)]
fn install_hook_entry(target: &Path, entry_path: &Path) -> Result<(), GitAiError> {
    std::os::unix::fs::symlink(target, entry_path)?;
    Ok(())
}

#[cfg(windows)]
fn install_hook_entry(target: &Path, entry_path: &Path) -> Result<(), GitAiError> {
    fs::copy(target, entry_path)
        .map(|_| ())
        .map_err(GitAiError::IoError)
}

#[cfg(windows)]
fn files_match_by_content(left: &Path, right: &Path) -> Result<bool, GitAiError> {
    let left_meta = fs::metadata(left)?;
    let right_meta = fs::metadata(right)?;

    if !left_meta.is_file() || !right_meta.is_file() {
        return Ok(false);
    }
    if left_meta.len() != right_meta.len() {
        return Ok(false);
    }

    let mut left_file = std::io::BufReader::new(fs::File::open(left)?);
    let mut right_file = std::io::BufReader::new(fs::File::open(right)?);
    let mut left_buf = [0u8; 8192];
    let mut right_buf = [0u8; 8192];

    loop {
        let left_read = left_file.read(&mut left_buf)?;
        let right_read = right_file.read(&mut right_buf)?;

        if left_read != right_read {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
        if left_buf[..left_read] != right_buf[..right_read] {
            return Ok(false);
        }
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn success_exit_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success_exit_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

fn repo_hook_state_schema_version() -> String {
    REPO_HOOK_STATE_SCHEMA_VERSION.to_string()
}

fn rebase_hook_mask_state_schema_version() -> String {
    REBASE_HOOK_MASK_STATE_SCHEMA_VERSION.to_string()
}

fn cherry_pick_batch_state_schema_version() -> String {
    CHERRY_PICK_BATCH_STATE_SCHEMA_VERSION.to_string()
}

fn repo_state_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(REPO_HOOK_STATE_FILE)
}

fn repo_enablement_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(REPO_HOOK_ENABLEMENT_FILE)
}

fn rebase_hook_mask_state_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(REBASE_HOOK_MASK_STATE_FILE)
}

fn managed_git_hooks_dir_for_repo(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(GIT_HOOKS_DIR_NAME)
}

fn managed_git_hooks_dir_from_context() -> Option<PathBuf> {
    git_dir_from_context().map(|git_dir| git_dir.join("ai").join(GIT_HOOKS_DIR_NAME))
}

fn stash_reference_transaction_state_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(STASH_REF_TX_STATE_FILE)
}

fn cherry_pick_batch_state_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(CHERRY_PICK_BATCH_STATE_FILE)
}

fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn canonicalize_if_possible(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

fn path_is_inside_managed_hooks(path: &Path, managed_hooks_dir: &Path) -> bool {
    normalize_path(path).starts_with(normalize_path(managed_hooks_dir))
}

fn path_looks_like_git_ai_binary(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };

    let stem = stem.to_ascii_lowercase();
    stem == "git"
        || stem == "git-ai"
        || stem == "git_ai"
        || stem.starts_with("git-ai-")
        || stem.starts_with("git_ai-")
}

fn resolve_repo_hook_binary_path(
    managed_hooks_dir: &Path,
    prior_state: Option<&RepoHookState>,
    current_exe_path: Option<PathBuf>,
) -> PathBuf {
    if let Some(current_exe) = current_exe_path.as_ref()
        && current_exe.exists()
        && !path_is_inside_managed_hooks(current_exe, managed_hooks_dir)
        && path_looks_like_git_ai_binary(current_exe)
    {
        return current_exe.clone();
    }

    if let Some(saved_path) = prior_state
        .map(|state| state.binary_path.trim())
        .filter(|path| !path.is_empty())
    {
        let saved_path = canonicalize_if_possible(PathBuf::from(saved_path));
        if saved_path.exists() && !path_is_inside_managed_hooks(&saved_path, managed_hooks_dir) {
            return saved_path;
        }
    }

    if let Some(current_exe) = current_exe_path
        && path_looks_like_git_ai_binary(&current_exe)
    {
        return current_exe;
    }

    PathBuf::from("git-ai")
}

fn resolved_current_exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok().map(canonicalize_if_possible)
}

fn is_managed_hooks_path(path: &Path, repo: Option<&Repository>) -> bool {
    if let Some(repo) = repo {
        return normalize_path(path) == normalize_path(&managed_git_hooks_dir_for_repo(repo));
    }
    if let Some(managed_from_context) = managed_git_hooks_dir_from_context() {
        return normalize_path(path) == normalize_path(&managed_from_context);
    }
    false
}

fn hook_perf_json_logging_enabled() -> bool {
    std::env::var("GIT_AI_DEBUG_PERFORMANCE")
        .ok()
        .and_then(|value| value.trim().parse::<u8>().ok())
        .map(|level| level >= 2)
        .unwrap_or(false)
}

fn global_git_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("GIT_CONFIG_GLOBAL")
        && !path.trim().is_empty()
    {
        return PathBuf::from(path);
    }
    crate::mdm::utils::home_dir().join(".gitconfig")
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
    let local_config_path = repo.path().join("config");
    if read_hooks_path_from_config(&local_config_path, gix_config::Source::Local).is_none() {
        return Ok(false);
    }

    if !dry_run {
        let mut args = repo.global_args_for_exec();
        args.extend(
            [
                "config",
                "--local",
                "--unset-all",
                CONFIG_KEY_CORE_HOOKS_PATH,
            ]
            .iter()
            .map(|value| value.to_string()),
        );
        crate::git::repository::exec_git(&args)?;
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

fn save_repo_hook_state(
    path: &Path,
    state: &RepoHookState,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let current = read_repo_hook_state(path).ok().flatten();
    if current.as_ref() == Some(state) {
        return Ok(false);
    }

    if !dry_run {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(state)?;
        fs::write(path, json)?;
    }

    Ok(true)
}

fn read_rebase_hook_mask_state(path: &Path) -> Result<Option<RebaseHookMaskState>, GitAiError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    match serde_json::from_str::<RebaseHookMaskState>(&content) {
        Ok(state) => Ok(Some(state)),
        Err(err) => {
            debug_log(&format!(
                "ignoring invalid rebase hook mask state {}: {}",
                path.display(),
                err
            ));
            Ok(None)
        }
    }
}

fn save_rebase_hook_mask_state(
    path: &Path,
    state: &RebaseHookMaskState,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let current = read_rebase_hook_mask_state(path).ok().flatten();
    if current.as_ref() == Some(state) {
        return Ok(false);
    }

    if !dry_run {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(state)?;
        fs::write(path, json)?;
    }

    Ok(true)
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

fn ensure_hook_entry_installed(
    hook_path: &Path,
    binary_path: &Path,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    if hook_path.exists() || hook_path.symlink_metadata().is_ok() {
        #[cfg(unix)]
        let should_replace = match fs::read_link(hook_path) {
            Ok(target) => normalize_path(&target) != normalize_path(binary_path),
            Err(_) => true,
        };

        #[cfg(windows)]
        let should_replace = match should_replace_windows_hook_entry(hook_path, binary_path) {
            Ok(should_replace) => should_replace,
            Err(err) => {
                if let GitAiError::IoError(io_err) = &err
                    && is_windows_lock_or_sharing_violation(io_err)
                {
                    debug_log(&format!(
                        "Deferring repo hook refresh for {} because it is currently in use",
                        hook_path.display()
                    ));
                    return Ok(false);
                }
                return Err(err);
            }
        };

        if should_replace {
            if !dry_run {
                #[cfg(windows)]
                {
                    if let Err(err) = remove_hook_entry(hook_path) {
                        if let GitAiError::IoError(io_err) = &err
                            && is_windows_lock_or_sharing_violation(io_err)
                        {
                            debug_log(&format!(
                                "Deferring repo hook refresh for {} because it is currently in use",
                                hook_path.display()
                            ));
                            return Ok(false);
                        }
                        return Err(err);
                    }
                }

                #[cfg(not(windows))]
                {
                    remove_hook_entry(hook_path)?;
                }
            }
        } else {
            return Ok(false);
        }
    }

    if !dry_run {
        #[cfg(windows)]
        {
            if let Err(err) = install_hook_entry(binary_path, hook_path) {
                if let GitAiError::IoError(io_err) = &err
                    && is_windows_lock_or_sharing_violation(io_err)
                {
                    // Defer only when an existing destination is still present/locked.
                    // If no hook file exists here, surfacing the error is safer than silently
                    // leaving the hook missing.
                    if hook_path.exists() || hook_path.symlink_metadata().is_ok() {
                        debug_log(&format!(
                            "Deferring repo hook refresh for {} because it is currently in use",
                            hook_path.display()
                        ));
                        return Ok(false);
                    }
                }
                return Err(err);
            }
        }

        #[cfg(not(windows))]
        {
            install_hook_entry(binary_path, hook_path)?;
        }
    }

    Ok(true)
}

#[cfg(windows)]
fn should_replace_windows_hook_entry(
    hook_path: &Path,
    binary_path: &Path,
) -> Result<bool, GitAiError> {
    let hook_metadata = hook_path.symlink_metadata()?;
    if !hook_metadata.file_type().is_file() {
        return Ok(true);
    }

    let source_metadata = fs::metadata(binary_path)?;
    if !source_metadata.file_type().is_file() {
        return Ok(true);
    }

    // Length mismatch is always stale.
    if hook_metadata.len() != source_metadata.len() {
        return Ok(true);
    }

    let hook_modified = hook_metadata.modified().ok();
    let source_modified = source_metadata.modified().ok();

    match (hook_modified, source_modified) {
        (Some(hook_ts), Some(source_ts)) if hook_ts < source_ts => Ok(true),
        // If hook appears newer (clock skew, timestamp granularity), verify bytes before skipping.
        (Some(hook_ts), Some(source_ts)) if hook_ts > source_ts => {
            Ok(!files_match_by_content(hook_path, binary_path)?)
        }
        // Equal timestamps are ambiguous on filesystems with coarse timestamp precision.
        _ => Ok(!files_match_by_content(hook_path, binary_path)?),
    }
}

#[cfg(windows)]
fn is_windows_lock_or_sharing_violation(io_err: &std::io::Error) -> bool {
    if let Some(code) = io_err.raw_os_error() {
        return matches!(code, 5 | 32 | 33);
    }

    io_err.kind() == std::io::ErrorKind::PermissionDenied
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

fn sync_non_managed_hook_entries(
    managed_hooks_dir: &Path,
    binary_path: &Path,
    forward_hooks_path: Option<&str>,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let mut changed = false;
    let forward_dir = forward_hooks_path.map(Path::new);

    for hook_name in CORE_GIT_HOOK_NAMES {
        if MANAGED_GIT_HOOK_NAMES.contains(hook_name) {
            continue;
        }
        let hook_path = managed_hooks_dir.join(hook_name);
        let original_exists = forward_dir
            .map(|d| d.join(hook_name))
            .is_some_and(|p| p.exists() && !p.is_dir());

        if original_exists {
            changed |= ensure_hook_entry_installed(&hook_path, binary_path, dry_run)?;
        } else if hook_path.exists() || hook_path.symlink_metadata().is_ok() {
            changed = true;
            if !dry_run {
                remove_hook_entry(&hook_path)?;
            }
        }
    }
    Ok(changed)
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
        let repo_ai_dir = repo.path().join("ai");
        if normalize_path(path).starts_with(normalize_path(&repo_ai_dir)) {
            return true;
        }
    }

    if let Some(managed_hooks_path) = managed_hooks_path
        && normalize_path(path) == normalize_path(managed_hooks_path)
    {
        return true;
    }

    is_managed_hooks_path(path, repo)
}

fn select_forward_target_for_repo(
    repo: &Repository,
    managed_hooks_dir: &Path,
    current_local_hooks: Option<&str>,
    prior_state: Option<&RepoHookState>,
) -> (ForwardMode, Option<String>, Option<String>) {
    if let Some(local_hooks) = current_local_hooks
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let local_path = PathBuf::from(local_hooks);
        if !is_disallowed_forward_hooks_path(&local_path, Some(repo), Some(managed_hooks_dir)) {
            return (
                ForwardMode::RepoLocal,
                Some(local_hooks.to_string()),
                Some(local_hooks.to_string()),
            );
        }
    }

    if let Some(state) = prior_state {
        if let Some(saved_forward_path) = state.forward_hooks_path.as_deref().map(str::trim)
            && !saved_forward_path.is_empty()
        {
            let saved_path = PathBuf::from(saved_forward_path);
            if !is_disallowed_forward_hooks_path(&saved_path, Some(repo), Some(managed_hooks_dir)) {
                return (
                    state.forward_mode.clone(),
                    Some(saved_forward_path.to_string()),
                    state.original_local_hooks_path.clone(),
                );
            }
        }
        if matches!(state.forward_mode, ForwardMode::None) {
            return (
                ForwardMode::None,
                None,
                state.original_local_hooks_path.clone(),
            );
        }
    }

    let global_hooks =
        read_hooks_path_from_config(&global_git_config_path(), gix_config::Source::User);
    if let Some(global_hooks) = global_hooks
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let global_path = PathBuf::from(global_hooks);
        if !is_disallowed_forward_hooks_path(&global_path, Some(repo), Some(managed_hooks_dir)) {
            return (
                ForwardMode::GlobalFallback,
                Some(global_hooks.to_string()),
                None,
            );
        }
    }

    (ForwardMode::None, None, None)
}

#[derive(Debug, Clone, Default)]
pub struct EnsureRepoHooksReport {
    pub changed: bool,
    pub managed_hooks_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct RemoveRepoHooksReport {
    pub changed: bool,
    pub managed_hooks_path: PathBuf,
}

pub fn ensure_repo_hooks_installed(
    repo: &Repository,
    dry_run: bool,
) -> Result<EnsureRepoHooksReport, GitAiError> {
    let managed_hooks_dir = managed_git_hooks_dir_for_repo(repo);
    let state_path = repo_state_path(repo);
    let local_config_path = repo.path().join("config");
    let prior_state = read_repo_hook_state(&state_path)?;

    let binary_path = resolve_repo_hook_binary_path(
        &managed_hooks_dir,
        prior_state.as_ref(),
        resolved_current_exe_path(),
    );
    let current_local_hooks =
        read_hooks_path_from_config(&local_config_path, gix_config::Source::Local);
    let (forward_mode, forward_hooks_path, original_local_hooks_path) =
        select_forward_target_for_repo(
            repo,
            &managed_hooks_dir,
            current_local_hooks.as_deref(),
            prior_state.as_ref(),
        );

    let mut changed = false;
    if !dry_run {
        fs::create_dir_all(&managed_hooks_dir)?;
    }

    for hook_name in MANAGED_GIT_HOOK_NAMES {
        let hook_path = managed_hooks_dir.join(hook_name);
        changed |= ensure_hook_entry_installed(&hook_path, &binary_path, dry_run)?;
    }

    changed |= sync_non_managed_hook_entries(
        &managed_hooks_dir,
        &binary_path,
        forward_hooks_path.as_deref(),
        dry_run,
    )?;

    changed |= set_hooks_path_in_config(
        &local_config_path,
        gix_config::Source::Local,
        &managed_hooks_dir.to_string_lossy(),
        dry_run,
    )?;

    let state = RepoHookState {
        schema_version: repo_hook_state_schema_version(),
        managed_hooks_path: managed_hooks_dir.to_string_lossy().to_string(),
        original_local_hooks_path,
        forward_mode,
        forward_hooks_path,
        binary_path: binary_path.to_string_lossy().to_string(),
    };
    changed |= save_repo_hook_state(&state_path, &state, dry_run)?;

    Ok(EnsureRepoHooksReport {
        changed,
        managed_hooks_path: managed_hooks_dir,
    })
}

pub fn remove_repo_hooks(
    repo: &Repository,
    dry_run: bool,
) -> Result<RemoveRepoHooksReport, GitAiError> {
    let managed_hooks_dir = managed_git_hooks_dir_for_repo(repo);
    let state_path = repo_state_path(repo);
    let enablement_path = repo_enablement_path(repo);
    let rebase_state_path = rebase_hook_mask_state_path(repo);
    let local_config_path = repo.path().join("config");
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

pub fn mark_repo_hooks_enabled(repo: &Repository) -> Result<bool, GitAiError> {
    let path = repo_enablement_path(repo);
    if path.exists() || path.symlink_metadata().is_ok() {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, b"enabled\n")?;
    Ok(true)
}

fn is_repo_hooks_enabled(repo: &Repository) -> bool {
    let path = repo_enablement_path(repo);
    path.exists() || path.symlink_metadata().is_ok()
}

static REPO_SELF_HEAL_GUARD: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

fn repo_lookup_path_for_self_heal(repo: &Repository) -> PathBuf {
    let repo_git_dir = repo.path().to_path_buf();
    match repo.is_bare_repository() {
        Ok(true) => repo_git_dir,
        Ok(false) | Err(_) => repo.workdir().unwrap_or(repo_git_dir),
    }
}

pub fn maybe_spawn_repo_hook_self_heal(repo: &Repository) {
    if !is_repo_hooks_enabled(repo) {
        return;
    }

    // Keep tests deterministic and avoid touching developer hook config during tests.
    if std::env::var("GIT_AI_TEST_DB_PATH").is_ok() {
        return;
    }

    let repo_git_dir = repo.path().to_path_buf();
    let repo_lookup_path = repo_lookup_path_for_self_heal(repo);
    let guard = REPO_SELF_HEAL_GUARD.get_or_init(|| Mutex::new(HashSet::new()));

    {
        let Ok(mut lock) = guard.lock() else {
            return;
        };
        if !lock.insert(repo_git_dir.clone()) {
            return;
        }
    }

    std::thread::spawn(move || {
        let result = (|| -> Result<(), GitAiError> {
            let repo = crate::git::find_repository_in_path(&repo_lookup_path.to_string_lossy())?;
            ensure_repo_hooks_installed(&repo, false).map(|_| ())
        })();

        if let Err(err) = result {
            debug_log(&format!("repo hook self-heal failed: {}", err));
        }

        if let Some(lock) = REPO_SELF_HEAL_GUARD
            .get()
            .and_then(|guard| guard.lock().ok())
        {
            let mut lock = lock;
            lock.remove(&repo_git_dir);
        }
    });
}

fn repo_state_path_from_env() -> Option<PathBuf> {
    git_dir_from_context().map(|git_dir| git_dir.join("ai").join(REPO_HOOK_STATE_FILE))
}

fn git_dir_from_env() -> Option<PathBuf> {
    let git_dir = std::env::var("GIT_DIR").ok()?;
    let git_dir = git_dir.trim();
    if git_dir.is_empty() {
        return None;
    }

    let git_dir = PathBuf::from(git_dir);
    if git_dir.is_absolute() {
        Some(git_dir)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(git_dir))
    }
}

fn git_dir_from_context() -> Option<PathBuf> {
    if let Some(from_env) = git_dir_from_env() {
        return Some(from_env);
    }

    // In some wrapper-internal invocations Git may not export GIT_DIR to hooks.
    // For normal non-bare hooks, the working directory is the repo root.
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join(".git");
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

fn hook_repository_lookup_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        paths.push(current_dir);
    }
    if let Some(git_dir) = git_dir_from_context() {
        if !paths
            .iter()
            .any(|existing| normalize_path(existing) == normalize_path(&git_dir))
        {
            paths.push(git_dir.clone());
        }
        if git_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.eq_ignore_ascii_case(".git"))
            .unwrap_or(false)
            && let Some(parent) = git_dir.parent()
        {
            let parent = parent.to_path_buf();
            if !paths
                .iter()
                .any(|existing| normalize_path(existing) == normalize_path(&parent))
            {
                paths.push(parent);
            }
        }
    }
    paths
}

fn find_hook_repository_from_context() -> Option<Repository> {
    hook_repository_lookup_paths()
        .into_iter()
        .find_map(|path| crate::git::find_repository_in_path(&path.to_string_lossy()).ok())
}

fn context_repo_ai_dir() -> Option<PathBuf> {
    git_dir_from_context().map(|git_dir| git_dir.join("ai"))
}

pub fn has_repo_hook_state(repo: Option<&Repository>) -> bool {
    let state_path = repo.map(repo_state_path).or_else(repo_state_path_from_env);
    state_path
        .map(|path| path.exists() || path.symlink_metadata().is_ok())
        .unwrap_or(false)
}

fn should_forward_repo_state_first(repo: Option<&Repository>) -> Option<PathBuf> {
    let state_path = repo
        .map(repo_state_path)
        .or_else(repo_state_path_from_env)?;
    let state = read_repo_hook_state(&state_path).ok().flatten()?;

    let managed_hooks_dir = if !state.managed_hooks_path.trim().is_empty() {
        Some(PathBuf::from(state.managed_hooks_path.trim()))
    } else if let Some(repo) = repo {
        Some(managed_git_hooks_dir_for_repo(repo))
    } else {
        managed_git_hooks_dir_from_context()
    };

    let fallback_repo = repo;
    let candidate = match state.forward_mode {
        ForwardMode::RepoLocal => state
            .forward_hooks_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        ForwardMode::GlobalFallback => {
            read_hooks_path_from_config(&global_git_config_path(), gix_config::Source::User)
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        }
        ForwardMode::None => None,
    }?;

    if is_disallowed_forward_hooks_path(&candidate, fallback_repo, managed_hooks_dir.as_deref()) {
        return None;
    }
    if let Some(context_repo_ai_dir) = context_repo_ai_dir()
        && normalize_path(&candidate).starts_with(normalize_path(&context_repo_ai_dir))
    {
        return None;
    }

    Some(candidate)
}

pub fn resolve_previous_non_managed_hooks_path(repo: Option<&Repository>) -> Option<PathBuf> {
    should_forward_repo_state_first(repo)
}

fn execute_forwarded_hook(
    hook_name: &str,
    hook_args: &[String],
    stdin_bytes: &[u8],
    repo: Option<&Repository>,
    cached_forward_dir: Option<PathBuf>,
) -> i32 {
    let Some(forward_hooks_dir) =
        cached_forward_dir.or_else(|| should_forward_repo_state_first(repo))
    else {
        return 0;
    };

    #[cfg(windows)]
    let mut hook_path = forward_hooks_dir.join(hook_name);

    #[cfg(not(windows))]
    let hook_path = forward_hooks_dir.join(hook_name);

    #[cfg(windows)]
    if !hook_path.exists() {
        let exe_candidate = forward_hooks_dir.join(format!("{}.exe", hook_name));
        if exe_candidate.exists() {
            hook_path = exe_candidate;
        }
    }

    if !hook_path.exists() || !is_executable(&hook_path) {
        return 0;
    }

    let mut cmd = Command::new(&hook_path);
    cmd.args(hook_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env(ENV_SKIP_ALL_HOOKS, "1");

    let Ok(mut child) = cmd.spawn() else {
        return 1;
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
    }

    let Ok(status) = child.wait() else {
        return 1;
    };
    status.code().unwrap_or(1)
}

fn rebase_masked_hook_path(managed_hooks_dir: &Path, hook_name: &str) -> PathBuf {
    managed_hooks_dir.join(format!("{}{}", hook_name, REBASE_HOOK_MASK_SUFFIX))
}

fn maybe_enable_rebase_hook_mask(repo: &Repository) {
    let state_path = rebase_hook_mask_state_path(repo);
    if read_rebase_hook_mask_state(&state_path)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }

    let managed_hooks_dir = managed_git_hooks_dir_for_repo(repo);
    let local_hooks_path =
        read_hooks_path_from_config(&repo.path().join("config"), gix_config::Source::Local)
            .map(|value| value.trim().to_string());
    if let Some(local_hooks_path) = local_hooks_path
        && normalize_path(Path::new(&local_hooks_path)) != normalize_path(&managed_hooks_dir)
    {
        return;
    }

    let mut masked_hooks = Vec::new();
    for hook_name in MANAGED_GIT_HOOK_NAMES {
        if REBASE_TERMINAL_HOOK_NAMES.contains(hook_name) {
            continue;
        }
        let hook_path = managed_hooks_dir.join(hook_name);
        if !(hook_path.exists() || hook_path.symlink_metadata().is_ok()) {
            continue;
        }
        let masked_path = rebase_masked_hook_path(&managed_hooks_dir, hook_name);
        if masked_path.exists() || masked_path.symlink_metadata().is_ok() {
            continue;
        }
        if fs::rename(&hook_path, &masked_path).is_ok() {
            masked_hooks.push((*hook_name).to_string());
        }
    }

    if masked_hooks.is_empty() {
        return;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let session_id = format!("{}-{}", std::process::id(), now_ms);
    let state = RebaseHookMaskState {
        schema_version: rebase_hook_mask_state_schema_version(),
        managed_hooks_path: managed_hooks_dir.to_string_lossy().to_string(),
        masked_hooks,
        active: true,
        session_id,
        created_at_unix_ms: now_ms as u64,
    };
    let _ = save_rebase_hook_mask_state(&state_path, &state, false);
}

fn restore_rebase_hooks_for_repo(repo: &Repository, force: bool) {
    if !force && is_rebase_in_progress(repo) {
        return;
    }

    let state_path = rebase_hook_mask_state_path(repo);
    let Some(state) = read_rebase_hook_mask_state(&state_path).ok().flatten() else {
        return;
    };

    let managed_hooks_dir = if !state.managed_hooks_path.trim().is_empty() {
        PathBuf::from(state.managed_hooks_path.trim())
    } else {
        managed_git_hooks_dir_for_repo(repo)
    };

    for hook_name in &state.masked_hooks {
        let masked_path = rebase_masked_hook_path(&managed_hooks_dir, hook_name);
        let hook_path = managed_hooks_dir.join(hook_name);
        if masked_path.exists() || masked_path.symlink_metadata().is_ok() {
            if hook_path.exists() || hook_path.symlink_metadata().is_ok() {
                let _ = fs::remove_file(&hook_path);
            }
            let _ = fs::rename(masked_path, hook_path);
        }
    }
    let _ = delete_state_file(&state_path, false);
}

fn maybe_restore_stale_rebase_hooks(repo: &Repository) {
    let state_path = rebase_hook_mask_state_path(repo);
    if !state_path.exists() {
        return;
    }
    if !is_rebase_in_progress(repo) {
        restore_rebase_hooks_for_repo(repo, true);
    }
}

fn force_restore_rebase_hooks(repo: &Repository) {
    restore_rebase_hooks_for_repo(repo, true);
}

fn parse_whitespace_fields(stdin: &[u8], min_fields: usize) -> Vec<Vec<String>> {
    String::from_utf8_lossy(stdin)
        .lines()
        .filter_map(|line| {
            let fields: Vec<String> = line.split_whitespace().map(String::from).collect();
            if fields.len() >= min_fields {
                Some(fields)
            } else {
                None
            }
        })
        .collect()
}

fn parse_hook_stdin(stdin: &[u8]) -> Vec<(String, String)> {
    parse_whitespace_fields(stdin, 2)
        .into_iter()
        .map(|fields| (fields[0].clone(), fields[1].clone()))
        .collect()
}

fn is_valid_git_oid(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_valid_git_oid_or_abbrev(value: &str) -> bool {
    value.len() >= 7 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_null_oid(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c == '0')
}

fn resolve_squash_source_head(repo: &Repository) -> Option<String> {
    // Some Git versions keep MERGE_HEAD for --squash, others do not.
    let merge_head_path = repo.path().join("MERGE_HEAD");
    if let Ok(contents) = fs::read_to_string(merge_head_path)
        && let Some(candidate) = contents
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
        && is_valid_git_oid(candidate)
    {
        return Some(candidate.to_string());
    }

    // SQUASH_MSG is created by `git merge --squash` and includes the squashed tip commit(s).
    // We use the first commit entry, which corresponds to the source head.
    let squash_msg_path = repo.path().join("SQUASH_MSG");
    if let Ok(contents) = fs::read_to_string(squash_msg_path) {
        for line in contents.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("commit ")
                && let Some(candidate) = rest.split_whitespace().next()
                && is_valid_git_oid(candidate)
            {
                return Some(candidate.to_string());
            }
        }
    }

    None
}

fn parsed_invocation(command: &str, command_args: Vec<String>) -> ParsedGitInvocation {
    ParsedGitInvocation {
        global_args: Vec::new(),
        command: Some(command.to_string()),
        command_args,
        saw_end_of_opts: false,
        is_help: false,
    }
}

fn default_context() -> CommandHooksContext {
    CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    }
}

fn is_pull_reflog_action() -> bool {
    std::env::var("GIT_REFLOG_ACTION")
        .map(|action| action.starts_with("pull"))
        .unwrap_or(false)
}

fn is_rebase_abort_reflog_action() -> bool {
    std::env::var("GIT_REFLOG_ACTION")
        .map(|action| {
            let action = action.to_ascii_lowercase();
            action.contains("rebase (abort)") || action.contains("rebase --abort")
        })
        .unwrap_or(false)
}

fn is_cherry_pick_abort_reflog_action() -> bool {
    std::env::var("GIT_REFLOG_ACTION")
        .map(|action| {
            let action = action.to_ascii_lowercase();
            action.contains("cherry-pick (abort)") || action.contains("cherry-pick --abort")
        })
        .unwrap_or(false)
}

fn is_post_commit_amend(repo: &Repository) -> bool {
    if let Ok(action) = std::env::var("GIT_REFLOG_ACTION")
        && action.to_ascii_lowercase().contains("amend")
    {
        return true;
    }

    let Ok(previous_head) = repo.revparse_single("HEAD@{1}") else {
        return false;
    };
    let Ok(new_head) = repo.head().and_then(|head| head.target()) else {
        return false;
    };
    let Ok(new_commit) = repo.find_commit(new_head) else {
        return false;
    };

    if let Ok(new_first_parent) = new_commit.parent(0) {
        return previous_head.id() != new_first_parent.id();
    }

    // Root-commit amend has no parent; detect it by observing the previous HEAD was also a root.
    repo.find_commit(previous_head.id())
        .and_then(|commit| commit.parent_count())
        .map(|parent_count| parent_count == 0)
        .unwrap_or(false)
}

fn pull_hook_state_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(PULL_HOOK_STATE_FILE)
}

fn clear_pull_hook_state(repo: &Repository) {
    let _ = fs::remove_file(pull_hook_state_path(repo));
}

fn save_pull_hook_state(repo: &Repository, state: &PullHookState) {
    let path = pull_hook_state_path(repo);
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if let Ok(data) = serde_json::to_vec(state) {
        let _ = fs::write(path, data);
    }
}

fn load_pull_hook_state(repo: &Repository) -> Option<PullHookState> {
    let path = pull_hook_state_path(repo);
    let data = fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn fetch_notes_from_all_remotes(repo: &Repository) {
    if let Ok(remotes) = repo.remotes() {
        for remote in remotes {
            let _ = fetch_authorship_notes(repo, &remote);
        }
    }
}

fn was_fast_forward_pull(repository: &Repository, expected_new_head: &str) -> bool {
    let mut args = repository.global_args_for_exec();
    args.extend(
        ["reflog", "-1", "--format=%H %gs"]
            .iter()
            .map(|s| s.to_string()),
    );

    match crate::git::repository::exec_git(&args) {
        Ok(output) => {
            let output_str = String::from_utf8_lossy(&output.stdout);
            let output_str = output_str.trim();

            let Some((sha, subject)) = output_str.split_once(' ') else {
                return false;
            };

            if sha != expected_new_head {
                return false;
            }

            subject.starts_with("pull") && subject.ends_with(": Fast-forward")
        }
        Err(_) => false,
    }
}

fn parse_reference_transaction_stdin(stdin: &[u8]) -> Vec<(String, String, String)> {
    parse_whitespace_fields(stdin, 3)
        .into_iter()
        .map(|fields| (fields[0].clone(), fields[1].clone(), fields[2].clone()))
        .collect()
}

fn latest_head_reflog_subject(repository: &Repository) -> Option<String> {
    let mut args = repository.global_args_for_exec();
    args.extend(
        ["reflog", "-1", "--format=%gs"]
            .iter()
            .map(|s| s.to_string()),
    );
    let output = crate::git::repository::exec_git(&args).ok()?;
    let subject = String::from_utf8(output.stdout).ok()?;
    Some(subject.trim().to_string())
}

fn maybe_handle_reset_reference_transaction(
    repo: &mut Repository,
    hook_args: &[String],
    updates: &[(String, String, String)],
) {
    if hook_args.first().map(String::as_str) != Some("committed") {
        return;
    }

    if let Ok(action) = std::env::var("GIT_REFLOG_ACTION") {
        if !action.starts_with("reset:") {
            return;
        }
    } else {
        let Some(subject) = latest_head_reflog_subject(repo) else {
            return;
        };
        if !subject.starts_with("reset:") {
            return;
        }
    }

    let head_ref = repo
        .head()
        .ok()
        .and_then(|head| head.name().map(|name| name.to_string()))
        .unwrap_or_else(|| "HEAD".to_string());
    let head_update = updates
        .iter()
        .find(|(_, _, reference)| reference == &head_ref || reference == "HEAD")
        .cloned();
    let Some((old_head, new_head, _)) = head_update else {
        return;
    };

    if is_null_oid(&old_head) || is_null_oid(&new_head) {
        return;
    }

    if old_head == new_head {
        return;
    }

    let is_backward_reset = repo
        .merge_base(new_head.clone(), old_head.clone())
        .map(|merge_base| merge_base == new_head)
        .unwrap_or(false);
    if !is_backward_reset {
        return;
    }

    let has_uncommitted_changes = repo
        .get_staged_and_unstaged_filenames()
        .map(|paths| !paths.is_empty())
        .unwrap_or(false);

    if has_uncommitted_changes {
        let human_author = commit_hooks::get_commit_default_author(repo, &[]);
        let _ = crate::authorship::rebase_authorship::reconstruct_working_log_after_reset(
            repo,
            &new_head,
            &old_head,
            &human_author,
            None,
        );
    } else {
        let _ = repo.storage.delete_working_log_for_base_commit(&old_head);
    }
}

fn load_stash_reference_transaction_state(
    repo: &Repository,
) -> Option<StashReferenceTransactionState> {
    let path = stash_reference_transaction_state_path(repo);
    let data = fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_stash_reference_transaction_state(
    repo: &Repository,
    state: &StashReferenceTransactionState,
) {
    let path = stash_reference_transaction_state_path(repo);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_vec(state) {
        let _ = fs::write(path, data);
    }
}

fn clear_stash_reference_transaction_state(repo: &Repository) {
    let _ = fs::remove_file(stash_reference_transaction_state_path(repo));
}

fn stash_entry_count(repo: &Repository) -> Option<usize> {
    let mut args = repo.global_args_for_exec();
    args.extend(
        ["stash", "list", "--format=%H"]
            .iter()
            .map(|s| s.to_string()),
    );

    let output = crate::git::repository::exec_git(&args).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).lines().count())
}

fn has_working_tree_changes(repo: &Repository) -> bool {
    repo.get_staged_and_unstaged_filenames()
        .map(|paths| !paths.is_empty())
        .unwrap_or(false)
}

fn maybe_handle_stash_reference_transaction(
    repo: &mut Repository,
    hook_args: &[String],
    updates: &[(String, String, String)],
) {
    if !config::Config::get().feature_flags().rewrite_stash {
        clear_stash_reference_transaction_state(repo);
        return;
    }

    let phase = hook_args.first().map(String::as_str).unwrap_or("");
    let stash_update = updates
        .iter()
        .find(|(_, _, reference)| reference == "refs/stash")
        .cloned();

    if phase == "aborted" {
        clear_stash_reference_transaction_state(repo);
        return;
    }

    if phase == "prepared" {
        if stash_update.is_some() {
            let state = StashReferenceTransactionState {
                before_count: stash_entry_count(repo).unwrap_or(0),
            };
            save_stash_reference_transaction_state(repo, &state);
        }
        return;
    }

    if phase != "committed" {
        return;
    }

    let Some((old, new, _)) = stash_update else {
        clear_stash_reference_transaction_state(repo);
        return;
    };

    let before_state = load_stash_reference_transaction_state(repo);
    clear_stash_reference_transaction_state(repo);

    let before_count = before_state
        .as_ref()
        .map(|state| state.before_count)
        .unwrap_or_else(|| if is_null_oid(&old) { 0 } else { 1 });
    let after_count = stash_entry_count(repo).unwrap_or(before_count);

    let old_is_zero = is_null_oid(&old);
    let new_is_zero = is_null_oid(&new);

    if !new_is_zero && (old_is_zero || after_count > before_count) {
        // Stash push/save created a new stash entry. Persist authorship in stash notes.
        let parsed = parsed_invocation("stash", vec!["push".to_string()]);
        let context = default_context();
        stash_hooks::post_stash_hook(&context, &parsed, repo, success_exit_status());
        return;
    }

    if !old_is_zero && (new_is_zero || after_count < before_count) {
        // Stash pop/apply removed stash@{0}. Restore attributions using removed stash SHA.
        // Skip pure drop/clear flows where no file changes were applied.
        if !has_working_tree_changes(repo) {
            return;
        }

        let parsed = parsed_invocation("stash", vec!["pop".to_string()]);
        let mut context = default_context();
        context.stash_sha = Some(old);
        stash_hooks::post_stash_hook(&context, &parsed, repo, success_exit_status());
        return;
    }

    // Some Git versions emit non-zero -> non-zero refs/stash transitions for push/pop.
    // Fall back to reflog action hints if count-based detection is inconclusive.
    if !old_is_zero && !new_is_zero {
        let action = std::env::var("GIT_REFLOG_ACTION")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if action.contains("pop") || action.contains("apply") || action.contains("autostash") {
            if has_working_tree_changes(repo) {
                let parsed = parsed_invocation("stash", vec!["pop".to_string()]);
                let mut context = default_context();
                context.stash_sha = Some(old);
                stash_hooks::post_stash_hook(&context, &parsed, repo, success_exit_status());
            }
        } else {
            let parsed = parsed_invocation("stash", vec!["push".to_string()]);
            let context = default_context();
            stash_hooks::post_stash_hook(&context, &parsed, repo, success_exit_status());
        }
    }
}

fn is_rebase_in_progress(repo: &Repository) -> bool {
    repo.path().join("rebase-merge").is_dir() || repo.path().join("rebase-apply").is_dir()
}

fn pull_rebase_todo_is_empty(repo: &Repository) -> bool {
    let todo_path = repo.path().join("rebase-merge").join("git-rebase-todo");
    fs::read_to_string(todo_path)
        .map(|contents| contents.trim().is_empty())
        .unwrap_or(false)
}

fn maybe_capture_pull_pre_rebase_state(repo: &Repository) {
    if !is_pull_reflog_action() {
        return;
    }

    if let Ok(old_head) = repo.head().and_then(|head| head.target()) {
        save_pull_hook_state(repo, &PullHookState { old_head });
    }
}

fn maybe_handle_pull_post_merge(repo: &mut Repository) {
    if !is_pull_reflog_action() {
        return;
    }

    fetch_notes_from_all_remotes(repo);

    let Ok(new_head) = repo.head().and_then(|head| head.target()) else {
        return;
    };

    if !was_fast_forward_pull(repo, &new_head) {
        return;
    }

    let Ok(old_head_obj) = repo.revparse_single("HEAD@{1}") else {
        return;
    };
    let old_head = old_head_obj.id();
    if old_head == new_head {
        return;
    }

    let _ = repo.storage.rename_working_log(&old_head, &new_head);
}

fn maybe_handle_pull_post_rewrite(repo: &mut Repository) {
    if !is_pull_reflog_action() {
        return;
    }

    fetch_notes_from_all_remotes(repo);

    let Ok(new_head) = repo.head().and_then(|head| head.target()) else {
        clear_pull_hook_state(repo);
        return;
    };

    let old_head = load_pull_hook_state(repo)
        .map(|state| state.old_head)
        .or_else(|| repo.revparse_single("HEAD@{1}").ok().map(|obj| obj.id()));

    let Some(old_head) = old_head else {
        clear_pull_hook_state(repo);
        return;
    };

    if old_head == new_head {
        clear_pull_hook_state(repo);
        return;
    }

    // Preserve uncommitted attribution logs (including autostash/applied changes)
    // by moving the old-head working log to the new head after pull --rebase.
    let _ = repo.storage.rename_working_log(&old_head, &new_head);

    // In skipped-commit pulls (`noop`), Git may not emit post-rewrite and no rebased
    // commits are created. Avoid mapping upstream history as "new" commits.
    let is_noop_rebase = fs::read_to_string(repo.path().join("rebase-merge").join("done"))
        .map(|done| done.lines().all(|line| line.trim() == "noop"))
        .unwrap_or(false);
    if is_noop_rebase {
        let original_count =
            rebase_hooks::build_rebase_commit_mappings(repo, &old_head, &new_head, None)
                .map(|(original, _)| original.len())
                .unwrap_or(0);
        debug_log(&format!(
            "Commit mapping: {} original -> 0 new",
            original_count
        ));
        debug_log(&format!(
            "Pull rebase mappings: {} original -> 0 new commits",
            original_count
        ));
        clear_pull_hook_state(repo);
        return;
    }

    let onto_head = repo
        .revparse_single("@{upstream}")
        .and_then(|obj| obj.peel_to_commit())
        .map(|commit| commit.id())
        .ok();
    let (original_commits, new_commits) = match rebase_hooks::build_rebase_commit_mappings(
        repo,
        &old_head,
        &new_head,
        onto_head.as_deref(),
    ) {
        Ok(mappings) => mappings,
        Err(_) => {
            clear_pull_hook_state(repo);
            return;
        }
    };

    debug_log(&format!(
        "Pull rebase mappings: {} original -> {} new commits",
        original_commits.len(),
        new_commits.len()
    ));

    if original_commits.is_empty() || new_commits.is_empty() {
        clear_pull_hook_state(repo);
        return;
    }

    let rebase_event = crate::git::rewrite_log::RewriteLogEvent::rebase_complete(
        crate::git::rewrite_log::RebaseCompleteEvent::new(
            old_head,
            new_head,
            false,
            original_commits,
            new_commits,
        ),
    );

    let commit_author = commit_hooks::get_commit_default_author(repo, &[]);
    repo.handle_rewrite_log_event(rebase_event, commit_author, false, true);
    clear_pull_hook_state(repo);
}

fn cherry_pick_state_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join("cherry_pick_hook_state")
}

fn clear_cherry_pick_state(repo: &Repository) {
    let _ = fs::remove_file(cherry_pick_state_path(repo));
}

fn read_cherry_pick_batch_state(repo: &Repository) -> Option<CherryPickBatchState> {
    let path = cherry_pick_batch_state_path(repo);
    let content = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<CherryPickBatchState>(&content) {
        Ok(state) => Some(state),
        Err(err) => {
            debug_log(&format!(
                "ignoring invalid cherry-pick batch state: {}",
                err
            ));
            None
        }
    }
}

fn save_cherry_pick_batch_state(repo: &Repository, state: &CherryPickBatchState) {
    let path = cherry_pick_batch_state_path(repo);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(content) = serde_json::to_string_pretty(state) {
        let _ = fs::write(path, content);
    }
}

fn clear_cherry_pick_batch_state(repo: &Repository) {
    let _ = fs::remove_file(cherry_pick_batch_state_path(repo));
}

fn cherry_pick_todo_has_pending(repo: &Repository) -> bool {
    let todo_path = repo.path().join("sequencer").join("todo");
    fs::read_to_string(todo_path)
        .map(|todo| {
            todo.lines().any(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
        })
        .unwrap_or(false)
}

fn is_cherry_pick_in_progress(repo: &Repository) -> bool {
    repo.path().join("CHERRY_PICK_HEAD").is_file() || repo.path().join("sequencer").is_dir()
}

fn is_cherry_pick_terminal_step(repo: &Repository) -> bool {
    !cherry_pick_todo_has_pending(repo)
}

fn maybe_capture_cherry_pick_pre_commit_state(repo: &Repository) {
    let cherry_pick_head_path = repo.path().join("CHERRY_PICK_HEAD");
    let Ok(source_commit_raw) = fs::read_to_string(&cherry_pick_head_path) else {
        clear_cherry_pick_state(repo);
        return;
    };
    let source_commit = source_commit_raw.trim();
    if source_commit.is_empty() {
        clear_cherry_pick_state(repo);
        return;
    }

    let Ok(base_commit) = repo.head().and_then(|head| head.target()) else {
        return;
    };

    let state_path = cherry_pick_state_path(repo);
    if let Some(parent) = state_path.parent()
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }

    let _ = fs::write(state_path, format!("{}\n{}\n", source_commit, base_commit));
}

fn load_cherry_pick_state(repo: &Repository) -> Option<(String, String)> {
    let state = fs::read_to_string(cherry_pick_state_path(repo)).ok()?;
    let mut lines = state.lines();
    let source_commit = lines.next()?.trim().to_string();
    let base_commit = lines.next()?.trim().to_string();
    if source_commit.is_empty() || base_commit.is_empty() {
        return None;
    }
    Some((source_commit, base_commit))
}

fn latest_cherry_pick_source_from_sequencer(repo: &Repository) -> Option<String> {
    let done_path = repo.path().join("sequencer").join("done");
    let done = fs::read_to_string(done_path).ok()?;
    for line in done.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let _command = parts.next()?;
        let source = parts.next()?;
        if is_valid_git_oid_or_abbrev(source) {
            return Some(source.to_string());
        }
    }
    None
}

fn maybe_finalize_cherry_pick_batch_state(repo: &mut Repository, force: bool) {
    let Some(state) = read_cherry_pick_batch_state(repo) else {
        return;
    };
    if !force && !is_cherry_pick_terminal_step(repo) {
        return;
    }
    if state.mappings.is_empty() {
        clear_cherry_pick_batch_state(repo);
        clear_cherry_pick_state(repo);
        return;
    }

    let Ok(new_head) = repo.head().and_then(|head| head.target()) else {
        clear_cherry_pick_batch_state(repo);
        clear_cherry_pick_state(repo);
        return;
    };

    let source_commits: Vec<String> = state
        .mappings
        .iter()
        .map(|mapping| mapping.source_commit.clone())
        .collect();
    let new_commits: Vec<String> = state
        .mappings
        .iter()
        .map(|mapping| mapping.new_commit.clone())
        .collect();
    if source_commits.is_empty() || new_commits.is_empty() {
        clear_cherry_pick_batch_state(repo);
        clear_cherry_pick_state(repo);
        return;
    }

    let original_head = if state.initial_head.trim().is_empty() {
        source_commits[0].clone()
    } else {
        state.initial_head.clone()
    };

    let commit_author = commit_hooks::get_commit_default_author(repo, &[]);
    repo.handle_rewrite_log_event(
        crate::git::rewrite_log::RewriteLogEvent::cherry_pick_complete(
            crate::git::rewrite_log::CherryPickCompleteEvent::new(
                original_head,
                new_head.clone(),
                source_commits,
                new_commits,
            ),
        ),
        commit_author,
        false,
        true,
    );
    clear_cherry_pick_batch_state(repo);
    clear_cherry_pick_state(repo);
}

fn maybe_finalize_stale_cherry_pick_batch_state(repo: &mut Repository) {
    if !is_cherry_pick_in_progress(repo) {
        maybe_finalize_cherry_pick_batch_state(repo, true);
    }
}

fn maybe_record_cherry_pick_post_commit(repo: &mut Repository) {
    let Ok(new_head) = repo.head().and_then(|head| head.target()) else {
        clear_cherry_pick_state(repo);
        clear_cherry_pick_batch_state(repo);
        return;
    };
    let original_head = repo
        .find_commit(new_head.clone())
        .ok()
        .and_then(|commit| commit.parent(0).ok())
        .map(|parent| parent.id());
    let Some(original_head) = original_head else {
        clear_cherry_pick_state(repo);
        return;
    };

    let source_commit = fs::read_to_string(repo.path().join("CHERRY_PICK_HEAD"))
        .ok()
        .map(|contents| contents.trim().to_string())
        .filter(|sha| !sha.is_empty())
        .or_else(|| latest_cherry_pick_source_from_sequencer(repo))
        .or_else(|| {
            load_cherry_pick_state(repo).and_then(|(source, base)| {
                if base == original_head {
                    Some(source)
                } else {
                    None
                }
            })
        });

    let Some(source_commit) = source_commit else {
        return;
    };

    // In unusual states HEAD may still point at the source commit; skip self-maps.
    if source_commit == new_head {
        clear_cherry_pick_state(repo);
        return;
    }

    let mut batch_state = read_cherry_pick_batch_state(repo).unwrap_or(CherryPickBatchState {
        schema_version: cherry_pick_batch_state_schema_version(),
        initial_head: original_head.clone(),
        mappings: Vec::new(),
        active: true,
    });
    if batch_state.schema_version.trim().is_empty() {
        batch_state.schema_version = cherry_pick_batch_state_schema_version();
    }
    if batch_state.initial_head.trim().is_empty() {
        batch_state.initial_head = original_head;
    }
    if !matches!(
        batch_state.mappings.last(),
        Some(last) if last.source_commit == source_commit && last.new_commit == new_head
    ) {
        batch_state.mappings.push(CherryPickBatchMapping {
            source_commit,
            new_commit: new_head,
        });
    }
    batch_state.active = true;
    save_cherry_pick_batch_state(repo, &batch_state);
    clear_cherry_pick_state(repo);
    // Finalize immediately per cherry-picked commit to avoid sequencer timing
    // differences across Git versions/platforms.
    maybe_finalize_cherry_pick_batch_state(repo, true);
}

fn is_post_commit_for_cherry_pick(repo: &Repository) -> bool {
    if repo.path().join("CHERRY_PICK_HEAD").is_file() {
        return true;
    }

    if repo.path().join("sequencer").is_dir()
        && latest_cherry_pick_source_from_sequencer(repo).is_some()
    {
        return true;
    }

    let Some((_, base_commit)) = load_cherry_pick_state(repo) else {
        return false;
    };

    let Ok(new_head) = repo.head().and_then(|head| head.target()) else {
        return false;
    };
    let Ok(parent) = repo
        .find_commit(new_head)
        .and_then(|commit| commit.parent(0))
        .map(|parent| parent.id())
    else {
        return false;
    };

    parent == base_commit
}

fn handle_rebase_post_rewrite_from_stdin(repo: &mut Repository, stdin: &[u8]) {
    let mappings = parse_hook_stdin(stdin);
    if mappings.is_empty() {
        return;
    }

    let original_commits: Vec<String> = mappings.iter().map(|(old, _)| old.clone()).collect();
    let new_commits: Vec<String> = mappings.iter().map(|(_, new)| new.clone()).collect();

    debug_log(&format!(
        "Commit mapping: {} original -> {} new",
        original_commits.len(),
        new_commits.len()
    ));

    let original_head = original_commits.last().cloned().unwrap();
    let new_head = repo
        .head()
        .ok()
        .and_then(|head| head.target().ok())
        .unwrap_or_else(|| new_commits.last().cloned().unwrap_or_default());

    if new_head.is_empty() {
        return;
    }

    let rebase_event = crate::git::rewrite_log::RewriteLogEvent::rebase_complete(
        crate::git::rewrite_log::RebaseCompleteEvent::new(
            original_head,
            new_head,
            false,
            original_commits,
            new_commits,
        ),
    );
    let commit_author = commit_hooks::get_commit_default_author(repo, &[]);
    repo.handle_rewrite_log_event(rebase_event, commit_author, false, true);
}

fn run_managed_hook(
    hook_name: &str,
    hook_args: &[String],
    stdin: &[u8],
    repo: Option<&Repository>,
) -> i32 {
    let Some(repo) = repo else {
        return 0;
    };

    // Keep behavior consistent with wrapper allow/exclude filtering.
    if !config::Config::get().is_allowed_repository(&Some(repo.clone())) {
        return 0;
    }

    let mut repo = repo.clone();
    maybe_restore_stale_rebase_hooks(&repo);
    maybe_finalize_stale_cherry_pick_batch_state(&mut repo);

    match hook_name {
        "pre-commit" => {
            if is_rebase_in_progress(&repo) {
                return 0;
            }
            if is_cherry_pick_in_progress(&repo) {
                maybe_capture_cherry_pick_pre_commit_state(&repo);
                return 0;
            }
            maybe_capture_cherry_pick_pre_commit_state(&repo);
            let parsed = parsed_invocation("commit", vec![]);
            let _ = commit_hooks::commit_pre_command_hook(&parsed, &mut repo);
            0
        }
        "post-commit" => {
            if is_rebase_in_progress(&repo) {
                return 0;
            }
            if is_post_commit_for_cherry_pick(&repo) {
                maybe_record_cherry_pick_post_commit(&mut repo);
                return 0;
            }
            if is_post_commit_amend(&repo) {
                // For --amend, post-rewrite (amend) owns rewrite mapping.
                // This avoids duplicate rewrite-log events while still preserving
                // authorship: commit_amend handling reads working logs and rewrites
                // notes for the amended commit.
                return 0;
            }
            if let Ok(parent) = repo.revparse_single("HEAD^") {
                repo.pre_command_base_commit = Some(parent.id());
            }
            let parsed = parsed_invocation("commit", vec![]);
            let mut context = default_context();
            context.pre_commit_hook_result = Some(true);
            commit_hooks::commit_post_command_hook(
                &parsed,
                success_exit_status(),
                &mut repo,
                &mut context,
            );
            0
        }
        "pre-rebase" => {
            maybe_enable_rebase_hook_mask(&repo);
            if is_pull_reflog_action() {
                maybe_capture_pull_pre_rebase_state(&repo);
            } else {
                let parsed = parsed_invocation("rebase", hook_args.to_vec());
                let mut context = default_context();
                rebase_hooks::pre_rebase_hook(&parsed, &mut repo, &mut context);
            }
            0
        }
        "post-rewrite" => {
            let rewrite_kind = hook_args.first().map(String::as_str).unwrap_or("");
            if rewrite_kind == "rebase" {
                if is_pull_reflog_action() {
                    maybe_handle_pull_post_rewrite(&mut repo);
                } else {
                    handle_rebase_post_rewrite_from_stdin(&mut repo, stdin);
                }
                // We may have temporarily disabled chatty hook entrypoints during rebase.
                // Restore them when we reach the terminal rebase rewrite hook.
                force_restore_rebase_hooks(&repo);
            } else if rewrite_kind == "amend" {
                // During interactive rebase flows, amend rewrite events are intermediate.
                // Let the final rebase post-rewrite event own attribution remapping.
                if is_rebase_in_progress(&repo) {
                    return 0;
                }
                for (old_sha, new_sha) in parse_hook_stdin(stdin) {
                    let commit_author = commit_hooks::get_commit_default_author(&repo, &[]);
                    repo.handle_rewrite_log_event(
                        crate::git::rewrite_log::RewriteLogEvent::commit_amend(old_sha, new_sha),
                        commit_author,
                        false,
                        true,
                    );
                }
            }
            0
        }
        "post-checkout" => {
            if hook_args.len() >= 2 {
                let old_head = hook_args[0].clone();
                let new_head = hook_args[1].clone();
                repo.pre_command_base_commit = Some(old_head);
                let is_pull_rebase_checkout =
                    is_pull_reflog_action() && repo.path().join("rebase-merge").is_dir();

                if !is_pull_rebase_checkout {
                    let parsed = parsed_invocation("checkout", vec![]);
                    let mut context = default_context();
                    checkout_hooks::post_checkout_hook(
                        &parsed,
                        &mut repo,
                        success_exit_status(),
                        &mut context,
                    );
                }

                // During clone, post-checkout typically runs once with an all-zero old sha.
                if is_null_oid(&hook_args[0]) && !is_null_oid(&new_head) {
                    let _ = fetch_authorship_notes(&repo, "origin");
                }

                // In pull --rebase when all local commits are skipped as duplicates,
                // Git may not invoke post-rewrite. The rebase todo is empty (noop case),
                // so run pull post-rewrite handling from post-checkout as a fallback.
                if is_pull_reflog_action()
                    && repo.path().join("rebase-merge").is_dir()
                    && pull_rebase_todo_is_empty(&repo)
                {
                    maybe_handle_pull_post_rewrite(&mut repo);
                    // `pull --rebase` skip/no-op flows may not emit post-rewrite.
                    force_restore_rebase_hooks(&repo);
                }

                // `git rebase --abort` exits via checkout and does not emit post-rewrite.
                // Restore the default hook profile after terminal abort checkout.
                if is_rebase_abort_reflog_action() {
                    force_restore_rebase_hooks(&repo);
                }

                if is_cherry_pick_abort_reflog_action() {
                    clear_cherry_pick_state(&repo);
                    clear_cherry_pick_batch_state(&repo);
                }
            }
            0
        }
        "post-merge" => {
            let mut args = Vec::new();
            if hook_args.first().map(String::as_str) == Some("1") {
                args.push("--squash".to_string());
                if let Some(source_head) = resolve_squash_source_head(&repo) {
                    args.push(source_head);
                } else {
                    debug_log("Could not resolve squash source head from MERGE_HEAD/SQUASH_MSG");
                    return 0;
                }
            }
            let parsed = parsed_invocation("merge", args);
            merge_hooks::post_merge_hook(&parsed, success_exit_status(), &mut repo);
            maybe_handle_pull_post_merge(&mut repo);
            0
        }
        "pre-push" => {
            let parsed = parsed_invocation("push", hook_args.to_vec());
            push_hooks::run_pre_push_hook_managed(&parsed, &repo);
            0
        }
        "reference-transaction" => {
            let updates = parse_reference_transaction_stdin(stdin);
            if updates.is_empty() {
                return 0;
            }
            maybe_handle_stash_reference_transaction(&mut repo, hook_args, &updates);
            maybe_handle_reset_reference_transaction(&mut repo, hook_args, &updates);
            0
        }
        "prepare-commit-msg" => {
            if is_rebase_in_progress(&repo) {
                return 0;
            }
            maybe_capture_cherry_pick_pre_commit_state(&repo);
            0
        }
        _ => 0,
    }
}

pub fn is_git_hook_binary_name(binary_name: &str) -> bool {
    CORE_GIT_HOOK_NAMES.contains(&binary_name)
}

fn needs_prepare_commit_msg_handling() -> bool {
    let Some(git_dir) = git_dir_from_context() else {
        // Keep existing behavior if git did not provide GIT_DIR in env.
        return true;
    };

    git_dir.join("CHERRY_PICK_HEAD").is_file()
}

fn is_rebase_in_progress_from_context() -> bool {
    let Some(git_dir) = git_dir_from_context() else {
        return false;
    };
    git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir()
}

fn hook_has_no_managed_behavior(hook_name: &str) -> bool {
    !MANAGED_GIT_HOOK_NAMES.contains(&hook_name)
}

fn hook_requires_managed_repo_lookup(
    hook_name: &str,
    hook_args: &[String],
    stdin_data: &[u8],
) -> bool {
    match hook_name {
        "pre-commit" | "post-commit" => !is_rebase_in_progress_from_context(),
        _ if hook_has_no_managed_behavior(hook_name) => false,
        "prepare-commit-msg" => {
            if is_rebase_in_progress_from_context() {
                return false;
            }
            needs_prepare_commit_msg_handling()
        }
        "reference-transaction" => {
            let phase = hook_args.first().map(String::as_str).unwrap_or("");
            let git_dir = git_dir_from_context();
            let in_rebase_or_cherry_pick = git_dir
                .as_ref()
                .map(|d| {
                    d.join("rebase-merge").is_dir()
                        || d.join("rebase-apply").is_dir()
                        || d.join("CHERRY_PICK_HEAD").is_file()
                        || d.join("sequencer").is_dir()
                })
                .unwrap_or(false);

            let has_stash_update = parse_whitespace_fields(stdin_data, 3)
                .iter()
                .any(|fields| fields.len() >= 3 && fields[2] == "refs/stash");

            if has_stash_update && config::Config::get().feature_flags().rewrite_stash {
                return matches!(phase, "prepared" | "committed" | "aborted");
            }

            if phase != "committed" {
                return false;
            }

            if let Ok(action) = std::env::var("GIT_REFLOG_ACTION") {
                return action.starts_with("reset:");
            }

            if in_rebase_or_cherry_pick {
                return false;
            }

            parse_whitespace_fields(stdin_data, 3).iter().any(|fields| {
                fields.len() >= 3 && (fields[2] == "HEAD" || fields[2].starts_with("refs/heads/"))
            })
        }
        _ => true,
    }
}

pub fn handle_git_hook_invocation(hook_name: &str, hook_args: &[String]) -> i32 {
    let perf_enabled = hook_perf_json_logging_enabled();
    let hook_start = perf_enabled.then(Instant::now);

    if std::env::var(ENV_SKIP_ALL_HOOKS).as_deref() == Ok("1") {
        return 0;
    }

    let skip_managed_hooks = std::env::var(ENV_SKIP_MANAGED_HOOKS).as_deref() == Ok("1")
        || std::env::var(ENV_SKIP_MANAGED_HOOKS_LEGACY).as_deref() == Ok("1");
    let cached_forward_dir = should_forward_repo_state_first(None);
    let forward_hooks_dir_exists = cached_forward_dir.is_some();

    // Fast path: child wrapper invocations in both mode set skip-managed-hooks.
    // If there is no forwarding target, this hook execution is guaranteed to be a no-op.
    if skip_managed_hooks && !forward_hooks_dir_exists {
        return 0;
    }

    // Fast path: if managed logic is a known no-op and there is no forwarding target,
    // we can avoid reading stdin and all filesystem/repository lookups.
    if hook_has_no_managed_behavior(hook_name) && !forward_hooks_dir_exists {
        return 0;
    }

    let mut stdin_data = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin_data);

    let mut repo = None;
    let mut lookup_ms = 0u128;
    let mut managed_ms = 0u128;

    if !skip_managed_hooks && hook_requires_managed_repo_lookup(hook_name, hook_args, &stdin_data) {
        let lookup_start = Instant::now();
        repo = find_hook_repository_from_context();
        lookup_ms = lookup_start.elapsed().as_millis();

        {
            let _guard = disable_internal_git_hooks();
            let managed_start = Instant::now();
            let managed_status = run_managed_hook(hook_name, hook_args, &stdin_data, repo.as_ref());
            managed_ms = managed_start.elapsed().as_millis();
            if managed_status != 0 {
                if perf_enabled {
                    debug_performance_log_structured(serde_json::json!({
                        "kind": "hook_invocation",
                        "hook": hook_name,
                        "managed_status": managed_status,
                        "repo_lookup_ms": lookup_ms,
                        "managed_ms": managed_ms,
                        "forward_ms": 0u128,
                        "total_ms": hook_start
                            .map(|start| start.elapsed().as_millis())
                            .unwrap_or(0),
                    }));
                }
                return managed_status;
            }
        }
    }

    let forward_start = Instant::now();
    let status = execute_forwarded_hook(
        hook_name,
        hook_args,
        &stdin_data,
        repo.as_ref(),
        cached_forward_dir,
    );
    let forward_ms = forward_start.elapsed().as_millis();
    if perf_enabled {
        debug_performance_log_structured(serde_json::json!({
            "kind": "hook_invocation",
            "hook": hook_name,
            "managed_status": 0,
            "forward_status": status,
            "repo_lookup_ms": lookup_ms,
            "managed_ms": managed_ms,
            "forward_ms": forward_ms,
            "total_ms": hook_start
                .map(|start| start.elapsed().as_millis())
                .unwrap_or(0),
        }));
    }
    status
}

pub fn ensure_repo_level_hooks_for_checkpoint(repo: &Repository) {
    maybe_spawn_repo_hook_self_heal(repo);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: tests below are marked serial to avoid concurrent env mutation.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: tests below are marked serial to avoid concurrent env mutation.
            unsafe {
                if let Some(old) = &self.old {
                    std::env::set_var(self.key, old);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn recognizes_hook_names() {
        assert!(is_git_hook_binary_name("pre-commit"));
        assert!(is_git_hook_binary_name("post-rewrite"));
        assert!(!is_git_hook_binary_name("git-ai"));
        assert!(!is_git_hook_binary_name("git"));
    }

    #[test]
    fn parse_post_rewrite_stdin() {
        let parsed = parse_hook_stdin(b"abc def\n111 222\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ("abc".to_string(), "def".to_string()));
        assert_eq!(parsed[1], ("111".to_string(), "222".to_string()));
    }

    fn init_repo(path: &Path) -> Repository {
        fs::create_dir_all(path).expect("failed to create repo dir");
        let isolated_home = path.join(".git-ai-test-home");
        fs::create_dir_all(&isolated_home).expect("failed to create isolated HOME for git init");
        let isolated_global_config = isolated_home.join(".gitconfig");
        fs::write(&isolated_global_config, "").expect("failed to create isolated global config");

        let init = Command::new("git")
            .args(["init", "."])
            .current_dir(path)
            .env("HOME", &isolated_home)
            .env("USERPROFILE", &isolated_home)
            .env("GIT_CONFIG_GLOBAL", &isolated_global_config)
            .output()
            .expect("failed to run git init");
        assert!(
            init.status.success(),
            "git init should succeed (status={:?}, stdout={}, stderr={})",
            init.status.code(),
            String::from_utf8_lossy(&init.stdout),
            String::from_utf8_lossy(&init.stderr)
        );
        crate::git::find_repository_in_path(&path.to_string_lossy())
            .expect("failed to open initialized repo")
    }

    #[test]
    fn ensure_repo_hooks_installed_uses_repo_local_forwarding() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        let user_hooks = tmp.path().join("repo-user-hooks");
        fs::create_dir_all(&user_hooks).expect("failed to create user hooks dir");

        let local_config = repo.path().join("config");
        set_hooks_path_in_config(
            &local_config,
            gix_config::Source::Local,
            &user_hooks.to_string_lossy(),
            false,
        )
        .expect("failed to set preexisting local hooksPath");

        let report =
            ensure_repo_hooks_installed(&repo, false).expect("ensure repo hooks should succeed");
        assert!(
            report.changed,
            "ensure should report updates on first install"
        );

        let managed_hooks_dir = managed_git_hooks_dir_for_repo(&repo);
        let configured_hooks_path =
            read_hooks_path_from_config(&local_config, gix_config::Source::Local)
                .expect("local hooksPath should be set");
        assert_eq!(
            normalize_path(Path::new(configured_hooks_path.trim())),
            normalize_path(&managed_hooks_dir),
            "local hooksPath should point to managed repo hooks"
        );

        for hook_name in MANAGED_GIT_HOOK_NAMES {
            let hook_path = managed_hooks_dir.join(hook_name);
            assert!(
                hook_path.exists() || hook_path.symlink_metadata().is_ok(),
                "managed hook should exist: {}",
                hook_name
            );
        }

        for hook_name in CORE_GIT_HOOK_NAMES {
            if MANAGED_GIT_HOOK_NAMES.contains(hook_name) {
                continue;
            }
            let hook_path = managed_hooks_dir.join(hook_name);
            assert!(
                !hook_path.exists() && hook_path.symlink_metadata().is_err(),
                "non-managed hook should NOT be provisioned when no original script exists in forward dir: {}",
                hook_name
            );
        }

        let state_path = repo_state_path(&repo);
        let state = read_repo_hook_state(&state_path)
            .expect("repo state should be readable")
            .expect("repo state should exist");
        assert_eq!(state.schema_version, REPO_HOOK_STATE_SCHEMA_VERSION);
        assert_eq!(state.forward_mode, ForwardMode::RepoLocal);
        assert_eq!(
            state.forward_hooks_path.as_deref(),
            Some(user_hooks.to_string_lossy().trim())
        );
        assert_eq!(
            state.original_local_hooks_path.as_deref(),
            Some(user_hooks.to_string_lossy().trim())
        );
    }

    #[test]
    #[serial]
    fn ensure_repo_hooks_installed_uses_global_fallback_when_local_missing() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("failed to create home dir");
        let global_config = home.join(".gitconfig");
        let global_hooks = tmp.path().join("global-hooks");
        fs::create_dir_all(&global_hooks).expect("failed to create global hooks dir");
        fs::write(
            &global_config,
            format!(
                "[core]\n\thooksPath = {}\n",
                global_hooks.to_string_lossy().replace('\\', "\\\\")
            ),
        )
        .expect("failed to write global config");

        let _home = EnvVarGuard::set("HOME", home.to_string_lossy().as_ref());
        let _global = EnvVarGuard::set(
            "GIT_CONFIG_GLOBAL",
            global_config.to_string_lossy().as_ref(),
        );

        let repo = init_repo(&tmp.path().join("repo"));
        let _ =
            ensure_repo_hooks_installed(&repo, false).expect("ensure repo hooks should succeed");

        let state_path = repo_state_path(&repo);
        let state = read_repo_hook_state(&state_path)
            .expect("repo state should be readable")
            .expect("repo state should exist");
        assert_eq!(state.forward_mode, ForwardMode::GlobalFallback);
        assert_eq!(
            state.forward_hooks_path.as_deref(),
            Some(global_hooks.to_string_lossy().trim())
        );
        assert!(
            state.original_local_hooks_path.is_none(),
            "original local hooks path should be empty when local hooksPath was unset"
        );
    }

    #[test]
    fn remove_repo_hooks_restores_preexisting_local_hooks_path() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        let user_hooks = tmp.path().join("user-hooks");
        fs::create_dir_all(&user_hooks).expect("failed to create user hooks dir");

        let local_config = repo.path().join("config");
        set_hooks_path_in_config(
            &local_config,
            gix_config::Source::Local,
            &user_hooks.to_string_lossy(),
            false,
        )
        .expect("failed to set preexisting hooksPath");

        ensure_repo_hooks_installed(&repo, false).expect("ensure should succeed");
        mark_repo_hooks_enabled(&repo).expect("opt-in marker should be writable");

        let remove_report =
            remove_repo_hooks(&repo, false).expect("remove repo hooks should succeed");
        assert!(remove_report.changed, "remove should report changes");

        let restored = read_hooks_path_from_config(&local_config, gix_config::Source::Local)
            .expect("local hooksPath should be restored");
        assert_eq!(
            normalize_path(Path::new(restored.trim())),
            normalize_path(&user_hooks),
            "local hooksPath should be restored to pre-ensure value"
        );

        let managed_hooks_dir = managed_git_hooks_dir_for_repo(&repo);
        assert!(
            !managed_hooks_dir.exists() && managed_hooks_dir.symlink_metadata().is_err(),
            "managed hooks directory should be removed"
        );

        let state_path = repo_state_path(&repo);
        assert!(
            !state_path.exists(),
            "repo hook state should be removed during remove"
        );

        let marker_path = repo_enablement_path(&repo);
        assert!(
            !marker_path.exists() && marker_path.symlink_metadata().is_err(),
            "repo hook opt-in marker should be removed during remove"
        );
    }

    #[test]
    fn remove_repo_hooks_unsets_local_hooks_path_when_no_original_value() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        let local_config = repo.path().join("config");

        ensure_repo_hooks_installed(&repo, false).expect("ensure should succeed");
        assert!(
            read_hooks_path_from_config(&local_config, gix_config::Source::Local).is_some(),
            "ensure should set local hooksPath"
        );

        let remove_report =
            remove_repo_hooks(&repo, false).expect("remove repo hooks should succeed");
        assert!(remove_report.changed, "remove should report changes");
        assert!(
            read_hooks_path_from_config(&local_config, gix_config::Source::Local).is_none(),
            "local hooksPath should be unset when there is no pre-ensure value"
        );
    }

    #[test]
    fn remove_repo_hooks_does_not_clobber_non_managed_current_hooks_path() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        let original_hooks = tmp.path().join("original-hooks");
        let replacement_hooks = tmp.path().join("replacement-hooks");
        fs::create_dir_all(&original_hooks).expect("failed to create original hooks dir");
        fs::create_dir_all(&replacement_hooks).expect("failed to create replacement hooks dir");

        let local_config = repo.path().join("config");
        set_hooks_path_in_config(
            &local_config,
            gix_config::Source::Local,
            &original_hooks.to_string_lossy(),
            false,
        )
        .expect("failed to set original hooksPath");

        ensure_repo_hooks_installed(&repo, false).expect("ensure should succeed");
        set_hooks_path_in_config(
            &local_config,
            gix_config::Source::Local,
            &replacement_hooks.to_string_lossy(),
            false,
        )
        .expect("failed to update hooksPath after ensure");

        remove_repo_hooks(&repo, false).expect("remove repo hooks should succeed");

        let local_hooks = read_hooks_path_from_config(&local_config, gix_config::Source::Local)
            .expect("replacement hooksPath should be preserved");
        assert_eq!(
            normalize_path(Path::new(local_hooks.trim())),
            normalize_path(&replacement_hooks),
            "remove should not overwrite non-managed current hooksPath"
        );
    }

    #[test]
    fn remove_repo_hooks_ignores_unexpected_managed_path_from_state() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        let repo_ai_dir = repo.path().join("ai");
        let survivor_file = repo_ai_dir.join("working_logs").join("survivor.json");
        fs::create_dir_all(
            survivor_file
                .parent()
                .expect("survivor file should have a parent"),
        )
        .expect("failed to create working_logs dir");
        fs::write(&survivor_file, b"{\"ok\":true}\n").expect("failed to create survivor file");

        ensure_repo_hooks_installed(&repo, false).expect("ensure should succeed");
        mark_repo_hooks_enabled(&repo).expect("opt-in marker should be writable");

        let poisoned_state = RepoHookState {
            schema_version: repo_hook_state_schema_version(),
            managed_hooks_path: repo_ai_dir.to_string_lossy().to_string(),
            original_local_hooks_path: None,
            forward_mode: ForwardMode::None,
            forward_hooks_path: None,
            binary_path: "git-ai".to_string(),
        };
        save_repo_hook_state(&repo_state_path(&repo), &poisoned_state, false)
            .expect("failed to write poisoned state");

        remove_repo_hooks(&repo, false).expect("remove repo hooks should succeed");

        assert!(
            survivor_file.exists(),
            "remove should not delete unrelated files under .git/ai"
        );
        assert!(
            !managed_git_hooks_dir_for_repo(&repo).exists(),
            "managed hooks dir should still be removed"
        );
    }

    #[test]
    fn forward_path_rejection_blocks_git_ai_managed_locations() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        let managed_hooks = managed_git_hooks_dir_for_repo(&repo);
        fs::create_dir_all(repo.path().join("ai")).expect("failed to create repo .git/ai dir");
        let repo_ai_path = repo.path().join("ai").join("other-hooks");
        fs::create_dir_all(&repo_ai_path).expect("failed to create repo-managed hooks candidate");
        let nested_git_ai = tmp.path().join(".git-ai").join("hooks");
        let foreign_git_ai = tmp
            .path()
            .join("foreign")
            .join(".git")
            .join("ai")
            .join("hooks");
        fs::create_dir_all(&foreign_git_ai).expect("failed to create foreign .git/ai candidate");
        let safe_target = tmp.path().join("external-hooks");

        assert!(is_disallowed_forward_hooks_path(
            &managed_hooks,
            Some(&repo),
            Some(&managed_hooks)
        ));
        assert!(is_disallowed_forward_hooks_path(
            &repo_ai_path,
            Some(&repo),
            Some(&managed_hooks)
        ));
        assert!(is_disallowed_forward_hooks_path(
            &nested_git_ai,
            Some(&repo),
            Some(&managed_hooks)
        ));
        assert!(is_disallowed_forward_hooks_path(
            &foreign_git_ai,
            Some(&repo),
            Some(&managed_hooks)
        ));
        assert!(!is_disallowed_forward_hooks_path(
            &safe_target,
            Some(&repo),
            Some(&managed_hooks)
        ));
    }

    #[test]
    fn rebase_hook_mask_roundtrip_restores_masked_hooks() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        ensure_repo_hooks_installed(&repo, false).expect("ensure repo hooks should succeed");

        maybe_enable_rebase_hook_mask(&repo);

        let state_path = rebase_hook_mask_state_path(&repo);
        let state = read_rebase_hook_mask_state(&state_path)
            .expect("rebase mask state should be readable")
            .expect("rebase mask state should exist");
        assert!(state.active, "rebase mask state should be active");
        assert_eq!(
            state.schema_version, REBASE_HOOK_MASK_STATE_SCHEMA_VERSION,
            "state schema should match"
        );
        assert!(
            state.masked_hooks.iter().any(|hook| hook == "pre-commit"),
            "expected pre-commit to be masked during rebase"
        );

        let managed_hooks_dir = managed_git_hooks_dir_for_repo(&repo);
        let masked_pre_commit = rebase_masked_hook_path(&managed_hooks_dir, "pre-commit");
        assert!(
            masked_pre_commit.exists() || masked_pre_commit.symlink_metadata().is_ok(),
            "masked pre-commit hook should be present"
        );
        assert!(
            !managed_hooks_dir.join("pre-commit").exists(),
            "pre-commit should be masked out of managed hooks during rebase"
        );

        restore_rebase_hooks_for_repo(&repo, true);
        assert!(
            !state_path.exists(),
            "rebase hook mask state should be removed after restore"
        );
        let restored_pre_commit = managed_hooks_dir.join("pre-commit");
        assert!(
            restored_pre_commit.exists() || restored_pre_commit.symlink_metadata().is_ok(),
            "pre-commit should be restored after rebase mask cleanup"
        );
    }

    #[test]
    fn repo_self_heal_lookup_uses_workdir_for_non_bare_repo() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo_dir = tmp.path().join("repo");
        fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
        let init = Command::new("git")
            .args(["init", "."])
            .current_dir(&repo_dir)
            .output()
            .expect("failed to run git init");
        assert!(init.status.success(), "git init should succeed");

        let repo = crate::git::find_repository_in_path(&repo_dir.to_string_lossy())
            .expect("failed to open initialized repo");
        let lookup_path = repo_lookup_path_for_self_heal(&repo);
        assert_eq!(
            normalize_path(&lookup_path),
            normalize_path(&repo_dir),
            "non-bare repos should self-heal using workdir path"
        );
    }

    #[test]
    fn repo_self_heal_opt_in_defaults_to_disabled() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        assert!(
            !is_repo_hooks_enabled(&repo),
            "self-heal should be disabled by default"
        );
    }

    #[test]
    fn mark_repo_hooks_enabled_is_idempotent() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));

        let first = mark_repo_hooks_enabled(&repo).expect("first opt-in should succeed");
        let second = mark_repo_hooks_enabled(&repo).expect("second opt-in should succeed");

        assert!(first, "first opt-in should report change");
        assert!(!second, "second opt-in should be a no-op");
        assert!(
            is_repo_hooks_enabled(&repo),
            "opt-in marker should be present"
        );
    }

    #[test]
    fn repo_self_heal_lookup_uses_git_dir_for_bare_repo() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let bare_dir = tmp.path().join("repo.git");
        let init = Command::new("git")
            .args(["init", "--bare", bare_dir.to_string_lossy().as_ref()])
            .output()
            .expect("failed to run git init --bare");
        assert!(init.status.success(), "git init --bare should succeed");

        let repo = crate::git::find_repository_in_path(&bare_dir.to_string_lossy())
            .expect("failed to open initialized bare repo");
        let lookup_path = repo_lookup_path_for_self_heal(&repo);
        assert_eq!(
            normalize_path(&lookup_path),
            normalize_path(repo.path()),
            "bare repos should self-heal using git dir path"
        );
    }

    #[test]
    fn valid_git_oid_accepts_sha1_and_sha256() {
        assert!(is_valid_git_oid("a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"));
        assert!(is_valid_git_oid(
            "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3a94a8fe5ccb19ba61c4c0873"
        ));
    }

    #[test]
    fn valid_git_oid_rejects_short_and_invalid() {
        assert!(!is_valid_git_oid("abcdef0"));
        assert!(!is_valid_git_oid(""));
        assert!(!is_valid_git_oid(
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        ));
        assert!(!is_valid_git_oid("abc"));
    }

    #[test]
    fn valid_git_oid_or_abbrev_accepts_short_hex() {
        assert!(is_valid_git_oid_or_abbrev("abcdef0"));
        assert!(is_valid_git_oid_or_abbrev(
            "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"
        ));
        assert!(!is_valid_git_oid_or_abbrev("abcde"));
        assert!(!is_valid_git_oid_or_abbrev(""));
        assert!(!is_valid_git_oid_or_abbrev("zzzzzzz"));
    }

    #[test]
    fn parse_reference_transaction_stdin_extracts_three_fields() {
        let input = b"0000000 1111111 refs/heads/main\naaa bbb refs/stash\n";
        let parsed = parse_reference_transaction_stdin(input);
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0],
            (
                "0000000".to_string(),
                "1111111".to_string(),
                "refs/heads/main".to_string()
            )
        );
        assert_eq!(
            parsed[1],
            (
                "aaa".to_string(),
                "bbb".to_string(),
                "refs/stash".to_string()
            )
        );
    }

    #[test]
    fn parse_reference_transaction_stdin_skips_incomplete_lines() {
        let input = b"only_one_field\ntwo fields\nold new ref\n";
        let parsed = parse_reference_transaction_stdin(input);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0],
            ("old".to_string(), "new".to_string(), "ref".to_string())
        );
    }

    #[test]
    fn parse_hook_stdin_skips_single_field_lines() {
        let input = b"only_one\nabc def\n\nghi jkl extra\n";
        let parsed = parse_hook_stdin(input);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ("abc".to_string(), "def".to_string()));
        assert_eq!(parsed[1], ("ghi".to_string(), "jkl".to_string()));
    }

    #[test]
    fn is_path_inside_component_finds_nested_segment() {
        assert!(is_path_inside_component(
            Path::new("/home/user/.git-ai/hooks"),
            ".git-ai"
        ));
        assert!(!is_path_inside_component(
            Path::new("/home/user/hooks"),
            ".git-ai"
        ));
        assert!(is_path_inside_component(
            Path::new("/a/.GIT-AI/b"),
            ".git-ai"
        ));
    }

    #[test]
    fn is_path_inside_any_git_ai_dir_detects_git_ai_subtree() {
        assert!(is_path_inside_any_git_ai_dir(Path::new(
            "/repo/.git/ai/hooks"
        )));
        assert!(!is_path_inside_any_git_ai_dir(Path::new(
            "/repo/.git/hooks"
        )));
        assert!(!is_path_inside_any_git_ai_dir(Path::new("/repo/ai/hooks")));
        assert!(is_path_inside_any_git_ai_dir(Path::new(
            "/other/.git/AI/deep/path"
        )));
    }

    #[test]
    fn hook_has_no_managed_behavior_classifies_correctly() {
        assert!(hook_has_no_managed_behavior("commit-msg"));
        assert!(hook_has_no_managed_behavior("pre-auto-gc"));
        assert!(hook_has_no_managed_behavior("fsmonitor-watchman"));
        assert!(!hook_has_no_managed_behavior("pre-commit"));
        assert!(!hook_has_no_managed_behavior("post-rewrite"));
        assert!(!hook_has_no_managed_behavior("reference-transaction"));
        assert!(!hook_has_no_managed_behavior("pre-push"));
    }

    #[test]
    fn cherry_pick_batch_state_serialization_roundtrip() {
        let state = CherryPickBatchState {
            schema_version: cherry_pick_batch_state_schema_version(),
            initial_head: "abc123".to_string(),
            mappings: vec![
                CherryPickBatchMapping {
                    source_commit: "aaa".to_string(),
                    new_commit: "bbb".to_string(),
                },
                CherryPickBatchMapping {
                    source_commit: "ccc".to_string(),
                    new_commit: "ddd".to_string(),
                },
            ],
            active: true,
        };

        let json = serde_json::to_string_pretty(&state).expect("serialization should succeed");
        let deserialized: CherryPickBatchState =
            serde_json::from_str(&json).expect("deserialization should succeed");
        assert_eq!(state, deserialized);
    }

    #[test]
    fn ensure_hook_entry_install_is_idempotent() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        ensure_repo_hooks_installed(&repo, false).expect("ensure repo hooks should succeed");

        let managed_hooks_dir = managed_git_hooks_dir_for_repo(&repo);
        let binary_path =
            resolve_repo_hook_binary_path(&managed_hooks_dir, None, resolved_current_exe_path());

        let hook_path = managed_hooks_dir.join("pre-commit");
        let first = ensure_hook_entry_installed(&hook_path, &binary_path, false)
            .expect("first ensure_hook_entry_installed");
        assert!(!first, "second call should report no change");

        let second = ensure_hook_entry_installed(&hook_path, &binary_path, false)
            .expect("second ensure_hook_entry_installed");
        assert!(!second, "third call should also report no change");
    }

    #[cfg(windows)]
    #[test]
    fn ensure_hook_entry_install_updates_copied_binary_when_source_is_newer() {
        use filetime::{FileTime, set_file_mtime};

        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let source_binary = tmp.path().join("source.exe");
        let hook_entry = tmp.path().join("pre-commit");

        fs::write(&source_binary, b"binary-v1").expect("failed to write source binary");
        let first = ensure_hook_entry_installed(&hook_entry, &source_binary, false)
            .expect("first install should succeed");
        assert!(first, "initial install should report change");

        fs::write(&source_binary, b"binary-v2").expect("failed to update source binary");
        let hook_mtime = FileTime::from_unix_time(1_700_000_000, 0);
        let source_mtime = FileTime::from_unix_time(1_700_000_010, 0);
        set_file_mtime(&hook_entry, hook_mtime).expect("failed to set hook mtime");
        set_file_mtime(&source_binary, source_mtime).expect("failed to set source mtime");

        let second = ensure_hook_entry_installed(&hook_entry, &source_binary, false)
            .expect("second install should succeed");
        assert!(second, "newer source should trigger replacement");

        let installed = fs::read(&hook_entry).expect("failed to read installed hook entry");
        assert_eq!(installed, b"binary-v2");
    }

    #[cfg(windows)]
    #[test]
    fn ensure_hook_entry_install_defers_when_hook_binary_is_locked() {
        use filetime::{FileTime, set_file_mtime};
        use std::os::windows::fs::OpenOptionsExt;

        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let source_binary = tmp.path().join("source.exe");
        let hook_entry = tmp.path().join("pre-commit");

        fs::write(&source_binary, b"binary-v1").expect("failed to write source binary");
        let first = ensure_hook_entry_installed(&hook_entry, &source_binary, false)
            .expect("first install should succeed");
        assert!(first, "initial install should report change");

        fs::write(&source_binary, b"binary-v2").expect("failed to update source binary");
        let hook_mtime = FileTime::from_unix_time(1_700_000_000, 0);
        let source_mtime = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_mtime(&hook_entry, hook_mtime).expect("failed to set hook mtime");
        set_file_mtime(&source_binary, source_mtime).expect("failed to set source mtime");

        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&hook_entry)
            .expect("failed to lock hook entry");

        let deferred = ensure_hook_entry_installed(&hook_entry, &source_binary, false)
            .expect("locked update should be deferred, not failed");
        assert!(!deferred, "locked hook should be deferred");

        drop(lock);

        let installed_after_unlock =
            fs::read(&hook_entry).expect("failed to read hook entry after lock release");
        assert_eq!(
            installed_after_unlock, b"binary-v1",
            "locked hook should keep previous contents until lock is released"
        );

        let retried = ensure_hook_entry_installed(&hook_entry, &source_binary, false)
            .expect("retry after lock release should succeed");
        assert!(retried, "update should apply after lock release");

        let installed_after_retry = fs::read(&hook_entry).expect("failed to read updated hook");
        assert_eq!(installed_after_retry, b"binary-v2");
    }

    #[test]
    fn resolve_repo_hook_binary_path_prefers_runtime_binary_over_saved_external_binary() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let managed_hooks_dir = tmp.path().join(".git").join("ai").join("hooks");
        fs::create_dir_all(&managed_hooks_dir).expect("failed to create managed hooks dir");

        let saved_binary = tmp.path().join("bin").join("saved-git-ai");
        fs::create_dir_all(saved_binary.parent().expect("saved binary parent"))
            .expect("failed to create saved binary parent");
        fs::write(&saved_binary, b"saved-binary").expect("failed to write saved binary");

        let runtime_binary = tmp.path().join("runtime").join("git-ai");
        fs::create_dir_all(runtime_binary.parent().expect("runtime binary parent"))
            .expect("failed to create runtime binary parent");
        fs::write(&runtime_binary, b"runtime-binary").expect("failed to write runtime binary");

        let state = RepoHookState {
            binary_path: saved_binary.to_string_lossy().to_string(),
            ..Default::default()
        };

        let resolved = resolve_repo_hook_binary_path(
            &managed_hooks_dir,
            Some(&state),
            Some(runtime_binary.clone()),
        );
        assert_eq!(
            normalize_path(&resolved),
            normalize_path(&runtime_binary),
            "runtime binary should be preferred when it is an external, valid path"
        );
    }

    #[test]
    fn resolve_repo_hook_binary_path_accepts_prefixed_git_ai_runtime_binary() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let managed_hooks_dir = tmp.path().join(".git").join("ai").join("hooks");
        fs::create_dir_all(&managed_hooks_dir).expect("failed to create managed hooks dir");

        let runtime_binary = tmp
            .path()
            .join("target")
            .join("debug")
            .join("git_ai-abcdef");
        fs::create_dir_all(runtime_binary.parent().expect("runtime binary parent"))
            .expect("failed to create runtime binary parent");
        fs::write(&runtime_binary, b"runtime-binary").expect("failed to write runtime binary");

        let resolved =
            resolve_repo_hook_binary_path(&managed_hooks_dir, None, Some(runtime_binary.clone()));
        assert_eq!(
            normalize_path(&resolved),
            normalize_path(&runtime_binary),
            "git_ai-* runtime binaries should be accepted as valid hook sources"
        );
    }

    #[test]
    fn resolve_repo_hook_binary_path_ignores_non_git_ai_runtime_binary() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let managed_hooks_dir = tmp.path().join(".git").join("ai").join("hooks");
        fs::create_dir_all(&managed_hooks_dir).expect("failed to create managed hooks dir");

        let saved_binary = tmp.path().join("bin").join("git-ai");
        fs::create_dir_all(saved_binary.parent().expect("saved binary parent"))
            .expect("failed to create saved binary parent");
        fs::write(&saved_binary, b"saved-binary").expect("failed to write saved binary");

        let runtime_binary = tmp.path().join("runtime").join("test-runner-binary");
        fs::create_dir_all(runtime_binary.parent().expect("runtime binary parent"))
            .expect("failed to create runtime binary parent");
        fs::write(&runtime_binary, b"runtime-binary").expect("failed to write runtime binary");

        let state = RepoHookState {
            binary_path: saved_binary.to_string_lossy().to_string(),
            ..Default::default()
        };

        let resolved = resolve_repo_hook_binary_path(
            &managed_hooks_dir,
            Some(&state),
            Some(runtime_binary.clone()),
        );
        assert_eq!(
            normalize_path(&resolved),
            normalize_path(&saved_binary),
            "non git-ai runtime binary should not override saved source binary"
        );
    }

    #[test]
    fn resolve_repo_hook_binary_path_prefers_prior_external_binary_when_runtime_inside_hooks() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let managed_hooks_dir = tmp.path().join(".git").join("ai").join("hooks");
        fs::create_dir_all(&managed_hooks_dir).expect("failed to create managed hooks dir");

        let saved_binary = tmp.path().join("bin").join("git-ai");
        fs::create_dir_all(saved_binary.parent().expect("saved binary parent"))
            .expect("failed to create saved binary parent");
        fs::write(&saved_binary, b"saved-binary").expect("failed to write saved binary");

        let runtime_binary = managed_hooks_dir.join("pre-commit");
        fs::write(&runtime_binary, b"runtime-binary").expect("failed to write runtime binary");

        let state = RepoHookState {
            binary_path: saved_binary.to_string_lossy().to_string(),
            ..Default::default()
        };

        let resolved = resolve_repo_hook_binary_path(
            &managed_hooks_dir,
            Some(&state),
            Some(runtime_binary.clone()),
        );
        assert_eq!(
            normalize_path(&resolved),
            normalize_path(&saved_binary),
            "saved external binary path should be preferred when runtime path is inside managed hooks"
        );
    }

    #[test]
    fn rebase_hook_mask_double_enable_is_noop() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        ensure_repo_hooks_installed(&repo, false).expect("ensure repo hooks should succeed");

        maybe_enable_rebase_hook_mask(&repo);

        let state_path = rebase_hook_mask_state_path(&repo);
        let state1 = read_rebase_hook_mask_state(&state_path)
            .expect("read should succeed")
            .expect("state should exist");

        maybe_enable_rebase_hook_mask(&repo);

        let state2 = read_rebase_hook_mask_state(&state_path)
            .expect("read should succeed")
            .expect("state should exist");
        assert_eq!(
            state1.session_id, state2.session_id,
            "second enable should not create a new session"
        );

        restore_rebase_hooks_for_repo(&repo, true);
    }

    #[test]
    fn repo_hook_state_serialization_roundtrip() {
        let state = RepoHookState {
            schema_version: repo_hook_state_schema_version(),
            managed_hooks_path: "/tmp/test/.git/ai/hooks".to_string(),
            original_local_hooks_path: Some("/tmp/user-hooks".to_string()),
            forward_mode: ForwardMode::RepoLocal,
            forward_hooks_path: Some("/tmp/user-hooks".to_string()),
            binary_path: "/usr/local/bin/git-ai".to_string(),
        };

        let json = serde_json::to_string_pretty(&state).expect("serialization should succeed");
        let deserialized: RepoHookState =
            serde_json::from_str(&json).expect("deserialization should succeed");
        assert_eq!(state, deserialized);
    }

    #[test]
    fn forward_mode_none_serialization() {
        let state = RepoHookState {
            forward_mode: ForwardMode::None,
            ..Default::default()
        };
        let json = serde_json::to_string(&state).expect("serialization should succeed");
        assert!(json.contains("\"none\""));
        let deserialized: RepoHookState =
            serde_json::from_str(&json).expect("deserialization should succeed");
        assert_eq!(deserialized.forward_mode, ForwardMode::None);
    }

    #[test]
    fn parse_whitespace_fields_handles_empty_input() {
        let empty: &[u8] = b"";
        assert!(parse_whitespace_fields(empty, 2).is_empty());
        assert!(parse_whitespace_fields(b"\n\n", 1).is_empty());
        assert!(parse_whitespace_fields(b"  \n  \n", 1).is_empty());
    }

    #[test]
    fn managed_git_hook_names_subset_of_core() {
        for name in MANAGED_GIT_HOOK_NAMES {
            assert!(
                CORE_GIT_HOOK_NAMES.contains(name),
                "managed hook {:?} should be in CORE_GIT_HOOK_NAMES",
                name
            );
        }
    }

    #[test]
    fn rebase_terminal_hooks_subset_of_managed() {
        for name in REBASE_TERMINAL_HOOK_NAMES {
            assert!(
                MANAGED_GIT_HOOK_NAMES.contains(name),
                "rebase terminal hook {:?} should be in MANAGED_GIT_HOOK_NAMES",
                name
            );
        }
    }

    #[test]
    #[serial]
    fn ensure_repo_hooks_no_forward_target_skips_non_managed() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let isolated_home = tmp.path().join("home");
        fs::create_dir_all(&isolated_home).expect("failed to create isolated home");
        let empty_global = isolated_home.join(".gitconfig");
        fs::write(&empty_global, "").expect("failed to write empty global config");
        let _home = EnvVarGuard::set("HOME", isolated_home.to_string_lossy().as_ref());
        let _global =
            EnvVarGuard::set("GIT_CONFIG_GLOBAL", empty_global.to_string_lossy().as_ref());

        let repo = init_repo(&tmp.path().join("repo"));

        let _ =
            ensure_repo_hooks_installed(&repo, false).expect("ensure repo hooks should succeed");

        let managed_hooks_dir = managed_git_hooks_dir_for_repo(&repo);
        for hook_name in MANAGED_GIT_HOOK_NAMES {
            let hook_path = managed_hooks_dir.join(hook_name);
            assert!(
                hook_path.exists() || hook_path.symlink_metadata().is_ok(),
                "managed hook should exist: {}",
                hook_name
            );
        }
        assert!(
            !managed_hooks_dir.join("commit-msg").exists(),
            "non-managed hooks should NOT be provisioned without a forward target"
        );
    }

    #[test]
    fn non_managed_hooks_provisioned_only_when_original_exists() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let repo = init_repo(&tmp.path().join("repo"));
        let user_hooks = tmp.path().join("user-hooks");
        fs::create_dir_all(&user_hooks).expect("failed to create user hooks dir");

        fs::write(user_hooks.join("commit-msg"), "#!/bin/sh\nexit 0\n")
            .expect("failed to write commit-msg hook");
        fs::write(user_hooks.join("pre-merge-commit"), "#!/bin/sh\nexit 0\n")
            .expect("failed to write pre-merge-commit hook");

        let local_config = repo.path().join("config");
        set_hooks_path_in_config(
            &local_config,
            gix_config::Source::Local,
            &user_hooks.to_string_lossy(),
            false,
        )
        .expect("failed to set preexisting local hooksPath");

        let _ =
            ensure_repo_hooks_installed(&repo, false).expect("ensure repo hooks should succeed");

        let managed_hooks_dir = managed_git_hooks_dir_for_repo(&repo);

        assert!(
            managed_hooks_dir
                .join("commit-msg")
                .symlink_metadata()
                .is_ok(),
            "commit-msg should be provisioned when original exists in forward dir"
        );
        assert!(
            managed_hooks_dir
                .join("pre-merge-commit")
                .symlink_metadata()
                .is_ok(),
            "pre-merge-commit should be provisioned when original exists in forward dir"
        );

        assert!(
            managed_hooks_dir
                .join("applypatch-msg")
                .symlink_metadata()
                .is_err(),
            "hooks without originals in forward dir should not be provisioned"
        );
    }

    #[test]
    fn non_managed_hook_entries_cleaned_on_resync() {
        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let managed_dir = tmp.path().join("managed");
        fs::create_dir_all(&managed_dir).expect("failed to create managed dir");
        let binary = tmp.path().join("fake-binary");
        fs::write(&binary, "").expect("failed to write fake binary");

        let forward_dir = tmp.path().join("forward");
        fs::create_dir_all(&forward_dir).expect("failed to create forward dir");
        fs::write(forward_dir.join("commit-msg"), "#!/bin/sh\nexit 0\n")
            .expect("failed to write commit-msg");

        let changed = sync_non_managed_hook_entries(
            &managed_dir,
            &binary,
            Some(forward_dir.to_string_lossy().as_ref()),
            false,
        )
        .expect("sync should succeed");
        assert!(changed, "first sync should report changes");
        assert!(
            managed_dir.join("commit-msg").symlink_metadata().is_ok(),
            "commit-msg hook entry should exist after sync"
        );

        fs::remove_file(forward_dir.join("commit-msg")).expect("failed to remove original");
        let changed = sync_non_managed_hook_entries(
            &managed_dir,
            &binary,
            Some(forward_dir.to_string_lossy().as_ref()),
            false,
        )
        .expect("resync should succeed");
        assert!(changed, "resync should report changes (stale removal)");
        assert!(
            managed_dir.join("commit-msg").symlink_metadata().is_err(),
            "commit-msg hook entry should be removed after original deleted"
        );
    }

    #[test]
    fn null_oid_detection() {
        assert!(is_null_oid("0000000000000000000000000000000000000000"));
        assert!(is_null_oid("0000000"));
        assert!(!is_null_oid(""));
        assert!(!is_null_oid("abc0000000000000000000000000000000000000"));
        assert!(!is_null_oid("0000000000000000000000000000000000000001"));
    }

    #[test]
    fn hook_has_no_managed_behavior_matches_managed_list() {
        for name in MANAGED_GIT_HOOK_NAMES {
            assert!(
                !hook_has_no_managed_behavior(name),
                "managed hook {:?} should NOT be classified as no-managed-behavior",
                name
            );
        }
        assert!(hook_has_no_managed_behavior("commit-msg"));
        assert!(hook_has_no_managed_behavior("pre-merge-commit"));
        assert!(hook_has_no_managed_behavior("fsmonitor-watchman"));
        assert!(hook_has_no_managed_behavior("totally-unknown-hook"));
    }

    #[test]
    fn hook_has_no_managed_behavior_consistent_with_core() {
        for name in CORE_GIT_HOOK_NAMES {
            if MANAGED_GIT_HOOK_NAMES.contains(name) {
                assert!(
                    !hook_has_no_managed_behavior(name),
                    "core+managed hook {:?} should have managed behavior",
                    name
                );
            } else {
                assert!(
                    hook_has_no_managed_behavior(name),
                    "core-only hook {:?} should have no managed behavior",
                    name
                );
            }
        }
    }
}
