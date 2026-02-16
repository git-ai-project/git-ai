use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::checkout_hooks;
use crate::commands::hooks::commit_hooks;
use crate::commands::hooks::merge_hooks;
use crate::commands::hooks::push_hooks;
use crate::commands::hooks::rebase_hooks;
use crate::config;
use crate::error::GitAiError;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::{Repository, disable_internal_git_hooks};
use crate::git::sync_authorship::fetch_authorship_notes;
use crate::utils::debug_log;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Mutex, OnceLock};

const CONFIG_KEY_CORE_HOOKS_PATH: &str = "core.hooksPath";
const GLOBAL_HOOK_STATE_FILE: &str = "global_git_hooks_state.json";
const REPO_HOOK_STATE_FILE: &str = "git_hooks_state.json";
const GIT_HOOKS_DIR_NAME: &str = "git-hooks";

pub const ENV_SKIP_ALL_HOOKS: &str = "GIT_AI_SKIP_ALL_HOOKS";
pub const ENV_SKIP_MANAGED_HOOKS: &str = "GIT_AI_SKIP_MANAGED_HOOKS";

// All core hooks we proxy/forward. We install every known hook name so global forwarding works
// even when git-ai doesn't have managed behavior for that hook.
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
    "pre-solve-refs",
];

#[allow(dead_code)]
pub fn core_git_hook_names() -> &'static [&'static str] {
    CORE_GIT_HOOK_NAMES
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HooksPathState {
    previous_hooks_path: String,
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> Result<(), GitAiError> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> Result<(), GitAiError> {
    std::os::windows::fs::symlink_file(target, link)
        .or_else(|_| std::fs::copy(target, link).map(|_| ()))
        .map_err(GitAiError::IoError)
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

fn managed_git_hooks_dir() -> PathBuf {
    if let Some(base) = config::git_ai_dir_path() {
        return base.join(GIT_HOOKS_DIR_NAME);
    }

    #[cfg(windows)]
    {
        crate::mdm::utils::home_dir()
            .join(".git-ai")
            .join(GIT_HOOKS_DIR_NAME)
    }

    #[cfg(not(windows))]
    {
        crate::mdm::utils::home_dir()
            .join(".git-ai")
            .join(GIT_HOOKS_DIR_NAME)
    }
}

fn global_state_path() -> Option<PathBuf> {
    config::internal_dir_path().map(|dir| dir.join(GLOBAL_HOOK_STATE_FILE))
}

fn repo_state_path(repo: &Repository) -> PathBuf {
    repo.path().join("ai").join(REPO_HOOK_STATE_FILE)
}

fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_managed_hooks_path(path: &Path) -> bool {
    let managed = managed_git_hooks_dir();
    normalize_path(path) == normalize_path(&managed)
}

fn is_managed_hooks_path_str(path: &str) -> bool {
    let as_path = PathBuf::from(path);
    if as_path.is_absolute() {
        return is_managed_hooks_path(&as_path);
    }

    as_path == managed_git_hooks_dir()
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

fn unset_hooks_path_in_config(
    path: &Path,
    source: gix_config::Source,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    let mut cfg = load_config(path, source)?;
    if cfg.string(CONFIG_KEY_CORE_HOOKS_PATH).is_none() {
        return Ok(false);
    }

    if !dry_run {
        if let Ok(mut raw) = cfg.raw_value_mut(&CONFIG_KEY_CORE_HOOKS_PATH) {
            raw.delete();
        }
        write_config(path, &cfg)?;
    }

    Ok(true)
}

fn save_hook_state(path: &Path, state: &HooksPathState, dry_run: bool) -> Result<bool, GitAiError> {
    let current = read_hook_state(path)
        .ok()
        .flatten()
        .map(|s| s.previous_hooks_path)
        .unwrap_or_default();

    if current == state.previous_hooks_path {
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

fn delete_hook_state(path: &Path, dry_run: bool) -> Result<bool, GitAiError> {
    if !path.exists() {
        return Ok(false);
    }

    if !dry_run {
        fs::remove_file(path)?;
    }
    Ok(true)
}

fn read_hook_state(path: &Path) -> Result<Option<HooksPathState>, GitAiError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let state = serde_json::from_str::<HooksPathState>(&content)?;
    Ok(Some(state))
}

fn ensure_hook_symlink(
    hook_path: &Path,
    binary_path: &Path,
    dry_run: bool,
) -> Result<bool, GitAiError> {
    if hook_path.exists() || hook_path.symlink_metadata().is_ok() {
        let should_replace = match fs::read_link(hook_path) {
            Ok(target) => normalize_path(&target) != normalize_path(binary_path),
            Err(_) => true,
        };

        if should_replace {
            if !dry_run {
                if hook_path.is_dir() {
                    fs::remove_dir_all(hook_path)?;
                } else {
                    fs::remove_file(hook_path)?;
                }
            }
        } else {
            return Ok(false);
        }
    }

    if !dry_run {
        create_file_symlink(binary_path, hook_path)?;
    }

    Ok(true)
}

pub fn install_global_git_hooks(binary_path: &Path, dry_run: bool) -> Result<bool, GitAiError> {
    let managed_dir = managed_git_hooks_dir();
    let global_cfg_path = global_git_config_path();
    let global_state = global_state_path();

    let mut changed = false;

    let existing_global_hooks =
        read_hooks_path_from_config(&global_cfg_path, gix_config::Source::User);
    if let Some(existing_hooks_path) = existing_global_hooks
        && !existing_hooks_path.trim().is_empty()
        && !is_managed_hooks_path_str(existing_hooks_path.trim())
        && let Some(state_path) = global_state
    {
        changed |= save_hook_state(
            &state_path,
            &HooksPathState {
                previous_hooks_path: existing_hooks_path,
            },
            dry_run,
        )?;
    }

    if !dry_run {
        fs::create_dir_all(&managed_dir)?;
    }

    for hook_name in CORE_GIT_HOOK_NAMES {
        let hook_path = managed_dir.join(hook_name);
        changed |= ensure_hook_symlink(&hook_path, binary_path, dry_run)?;
    }

    changed |= set_hooks_path_in_config(
        &global_cfg_path,
        gix_config::Source::User,
        &managed_dir.to_string_lossy(),
        dry_run,
    )?;

    Ok(changed)
}

pub fn uninstall_global_git_hooks(dry_run: bool) -> Result<bool, GitAiError> {
    let managed_dir = managed_git_hooks_dir();
    let global_cfg_path = global_git_config_path();
    let global_state = global_state_path();

    let mut changed = false;

    let current_global_hooks =
        read_hooks_path_from_config(&global_cfg_path, gix_config::Source::User);
    let current_is_managed = current_global_hooks
        .as_deref()
        .map(|value| is_managed_hooks_path_str(value.trim()))
        .unwrap_or(false);

    if current_is_managed {
        if let Some(state_path) = global_state.as_ref()
            && let Some(state) = read_hook_state(state_path)?
            && !state.previous_hooks_path.trim().is_empty()
            && !is_managed_hooks_path_str(state.previous_hooks_path.trim())
        {
            changed |= set_hooks_path_in_config(
                &global_cfg_path,
                gix_config::Source::User,
                state.previous_hooks_path.trim(),
                dry_run,
            )?;
        } else {
            changed |=
                unset_hooks_path_in_config(&global_cfg_path, gix_config::Source::User, dry_run)?;
        }
    }

    if let Some(state_path) = global_state {
        changed |= delete_hook_state(&state_path, dry_run)?;
    }

    if managed_dir.exists() {
        changed = true;
        if !dry_run {
            fs::remove_dir_all(managed_dir)?;
        }
    }

    Ok(changed)
}

static REPO_SELF_HEAL_GUARD: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

pub fn maybe_spawn_repo_hook_self_heal(repo: &Repository) {
    if !config::Config::get().get_feature_flags().global_git_hooks {
        return;
    }

    // Keep tests deterministic and avoid touching developer hook config during tests.
    if std::env::var("GIT_AI_TEST_DB_PATH").is_ok() {
        return;
    }

    let repo_git_dir = repo.path().to_path_buf();
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
            let repo = crate::git::find_repository_in_path(&repo_git_dir.to_string_lossy())?;
            ensure_repo_level_hooks_for_repo(&repo)
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

fn ensure_repo_level_hooks_for_repo(repo: &Repository) -> Result<(), GitAiError> {
    let managed_dir = managed_git_hooks_dir();
    if !managed_dir.exists() {
        return Ok(());
    }

    let local_config_path = repo.path().join("config");
    let current_local_hooks =
        read_hooks_path_from_config(&local_config_path, gix_config::Source::Local);

    // If no repo-level hooksPath is configured, do nothing.
    let Some(current_local_hooks) = current_local_hooks else {
        return Ok(());
    };

    if current_local_hooks.trim().is_empty() {
        return Ok(());
    }

    let repo_state = repo_state_path(repo);
    let current_is_managed = is_managed_hooks_path_str(current_local_hooks.trim());

    if !current_is_managed {
        save_hook_state(
            &repo_state,
            &HooksPathState {
                previous_hooks_path: current_local_hooks.clone(),
            },
            false,
        )?;
        let _ = set_hooks_path_in_config(
            &local_config_path,
            gix_config::Source::Local,
            &managed_dir.to_string_lossy(),
            false,
        )?;
        return Ok(());
    }

    // If already managed and we have a state file, keep as-is.
    if repo_state.exists() {
        return Ok(());
    }

    Ok(())
}

fn should_forward_repo_state_first(repo: Option<&Repository>) -> Option<PathBuf> {
    // Repo-level forwarding takes precedence over global forwarding exactly like
    // git's config precedence: if a repo has persisted hook state, never fall
    // back to global hook state for forwarding.
    if let Some(repo) = repo {
        let repo_state = repo_state_path(repo);
        if repo_state.exists() {
            if let Ok(Some(state)) = read_hook_state(&repo_state) {
                let candidate = PathBuf::from(state.previous_hooks_path);
                if !is_managed_hooks_path(&candidate) {
                    return Some(candidate);
                }
            }
            return None;
        }
    }

    if let Some(global_path) = global_state_path()
        && let Ok(Some(state)) = read_hook_state(&global_path)
    {
        let candidate = PathBuf::from(state.previous_hooks_path);
        if !is_managed_hooks_path(&candidate) {
            return Some(candidate);
        }
    }

    None
}

fn execute_forwarded_hook(
    hook_name: &str,
    hook_args: &[String],
    stdin_bytes: &[u8],
    repo: Option<&Repository>,
) -> i32 {
    let Some(forward_hooks_dir) = should_forward_repo_state_first(repo) else {
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(ENV_SKIP_ALL_HOOKS, "1");

    let Ok(mut child) = cmd.spawn() else {
        return 1;
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
    }

    let Ok(output) = child.wait_with_output() else {
        return 1;
    };

    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);

    output.status.code().unwrap_or(1)
}

fn parse_hook_stdin(stdin: &[u8]) -> Vec<(String, String)> {
    String::from_utf8_lossy(stdin)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let old_sha = parts.next()?;
            let new_sha = parts.next()?;
            Some((old_sha.to_string(), new_sha.to_string()))
        })
        .collect()
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

    match hook_name {
        "pre-commit" => {
            let parsed = parsed_invocation("commit", vec![]);
            if commit_hooks::commit_pre_command_hook(&parsed, &mut repo) {
                0
            } else {
                0
            }
        }
        "post-commit" => {
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
            let parsed = parsed_invocation("rebase", hook_args.to_vec());
            let mut context = default_context();
            rebase_hooks::pre_rebase_hook(&parsed, &mut repo, &mut context);
            0
        }
        "post-rewrite" => {
            let rewrite_kind = hook_args.first().map(String::as_str).unwrap_or("");
            if rewrite_kind == "rebase" {
                let parsed = parsed_invocation("rebase", vec![]);
                let context = default_context();
                rebase_hooks::handle_rebase_post_command(
                    &context,
                    &parsed,
                    success_exit_status(),
                    &mut repo,
                );
            } else if rewrite_kind == "amend" {
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

                let parsed = parsed_invocation("checkout", vec![]);
                let mut context = default_context();
                checkout_hooks::post_checkout_hook(
                    &parsed,
                    &mut repo,
                    success_exit_status(),
                    &mut context,
                );

                // During clone, post-checkout typically runs once with an all-zero old sha.
                if hook_args[0].chars().all(|c| c == '0') && !new_head.chars().all(|c| c == '0') {
                    let _ = fetch_authorship_notes(&repo, "origin");
                }
            }
            0
        }
        "post-merge" => {
            let mut args = Vec::new();
            if hook_args.first().map(String::as_str) == Some("1") {
                args.push("--squash".to_string());
            }
            let parsed = parsed_invocation("merge", args);
            merge_hooks::post_merge_hook(&parsed, success_exit_status(), &mut repo);
            0
        }
        "pre-push" => {
            let parsed = parsed_invocation("push", hook_args.to_vec());
            let mut context = default_context();
            context.push_authorship_handle = push_hooks::push_pre_command_hook(&parsed, &repo);
            push_hooks::push_post_command_hook(&repo, &parsed, success_exit_status(), &mut context);
            0
        }
        "post-fetch" => {
            if let Ok(remotes) = repo.remotes() {
                for remote in remotes {
                    let _ = fetch_authorship_notes(&repo, &remote);
                }
            }
            0
        }
        "reference-transaction" => {
            // Placeholder hook for future reset/stash/head-move reconstruction. No-op for now.
            0
        }
        "prepare-commit-msg"
        | "commit-msg"
        | "pre-merge-commit"
        | "pre-auto-gc"
        | "sendemail-validate"
        | "post-index-change"
        | "applypatch-msg"
        | "pre-applypatch"
        | "post-applypatch"
        | "pre-receive"
        | "update"
        | "proc-receive"
        | "post-receive"
        | "post-update"
        | "push-to-checkout"
        | "pre-solve-refs"
        | "fsmonitor-watchman"
        | "p4-changelist"
        | "p4-prepare-changelist"
        | "p4-post-changelist"
        | "p4-pre-submit" => 0,
        _ => 0,
    }
}

pub fn is_git_hook_binary_name(binary_name: &str) -> bool {
    CORE_GIT_HOOK_NAMES.contains(&binary_name)
}

pub fn handle_git_hook_invocation(hook_name: &str, hook_args: &[String]) -> i32 {
    let mut stdin_data = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin_data);

    let current_dir = match std::env::current_dir() {
        Ok(path) => path,
        Err(_) => PathBuf::from("."),
    };

    let repo = crate::git::find_repository_in_path(&current_dir.to_string_lossy()).ok();

    if std::env::var(ENV_SKIP_ALL_HOOKS).as_deref() == Ok("1") {
        return 0;
    }

    if std::env::var(ENV_SKIP_MANAGED_HOOKS).as_deref() != Ok("1") {
        let _guard = disable_internal_git_hooks();
        let managed_status = run_managed_hook(hook_name, hook_args, &stdin_data, repo.as_ref());
        if managed_status != 0 {
            return managed_status;
        }
    }

    execute_forwarded_hook(hook_name, hook_args, &stdin_data, repo.as_ref())
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

    #[cfg(unix)]
    #[test]
    #[serial]
    fn install_and_uninstall_global_hooks_roundtrip_restores_previous_path() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("failed to create temp home");

        let global_config = home.join(".gitconfig");
        fs::write(
            &global_config,
            "[core]\n\thooksPath = /tmp/original-hooks\n",
        )
        .expect("failed to write global config");

        let fake_binary = tmp.path().join("git-ai");
        fs::write(&fake_binary, "#!/bin/sh\nexit 0\n").expect("failed to write fake binary");
        let mut perms = fs::metadata(&fake_binary)
            .expect("failed to stat fake binary")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_binary, perms).expect("failed to chmod fake binary");

        let _home = EnvVarGuard::set("HOME", &home.to_string_lossy());
        let _global = EnvVarGuard::set("GIT_CONFIG_GLOBAL", &global_config.to_string_lossy());

        let installed =
            install_global_git_hooks(&fake_binary, false).expect("install should succeed");
        assert!(installed, "install should report changes");

        let configured_hooks_path =
            read_hooks_path_from_config(&global_config, gix_config::Source::User)
                .expect("global hooksPath should be set");
        assert!(
            configured_hooks_path.ends_with("/.git-ai/git-hooks"),
            "hooksPath should point at managed hooks dir"
        );

        let state_path = global_state_path().expect("global state path should resolve");
        let state = read_hook_state(&state_path)
            .expect("state read should succeed")
            .expect("state should be written");
        assert_eq!(state.previous_hooks_path, "/tmp/original-hooks");

        let uninstalled = uninstall_global_git_hooks(false).expect("uninstall should succeed");
        assert!(uninstalled, "uninstall should report changes");

        let restored_hooks_path =
            read_hooks_path_from_config(&global_config, gix_config::Source::User)
                .expect("global hooksPath should still be set");
        assert_eq!(restored_hooks_path, "/tmp/original-hooks");
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn repo_state_suppresses_global_forwarding_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("failed to create tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("failed to create temp home");
        let _home = EnvVarGuard::set("HOME", &home.to_string_lossy());

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

        let repo_hooks = tmp.path().join("repo-hooks");
        fs::create_dir_all(&repo_hooks).expect("failed to create repo hooks dir");
        let repo_hook = repo_hooks.join("pre-commit");
        fs::write(&repo_hook, "#!/bin/sh\nexit 0\n").expect("failed to write repo hook");
        let mut repo_perms = fs::metadata(&repo_hook)
            .expect("failed to stat repo hook")
            .permissions();
        repo_perms.set_mode(0o755);
        fs::set_permissions(&repo_hook, repo_perms).expect("failed to chmod repo hook");

        let global_hooks = tmp.path().join("global-hooks");
        fs::create_dir_all(&global_hooks).expect("failed to create global hooks dir");
        let global_hook = global_hooks.join("pre-commit");
        fs::write(&global_hook, "#!/bin/sh\nexit 0\n").expect("failed to write global hook");
        let mut global_perms = fs::metadata(&global_hook)
            .expect("failed to stat global hook")
            .permissions();
        global_perms.set_mode(0o755);
        fs::set_permissions(&global_hook, global_perms).expect("failed to chmod global hook");

        let repo_state = repo_state_path(&repo);
        fs::create_dir_all(
            repo_state
                .parent()
                .expect("repo state file should have parent"),
        )
        .expect("failed to create repo state parent");
        save_hook_state(
            &repo_state,
            &HooksPathState {
                previous_hooks_path: repo_hooks.to_string_lossy().to_string(),
            },
            false,
        )
        .expect("failed to save repo state");

        let global_state = global_state_path().expect("global state should resolve");
        fs::create_dir_all(
            global_state
                .parent()
                .expect("global state file should have parent"),
        )
        .expect("failed to create global state parent");
        save_hook_state(
            &global_state,
            &HooksPathState {
                previous_hooks_path: global_hooks.to_string_lossy().to_string(),
            },
            false,
        )
        .expect("failed to save global state");

        let resolved =
            should_forward_repo_state_first(Some(&repo)).expect("repo forward path should exist");
        assert_eq!(
            normalize_path(&resolved),
            normalize_path(&repo_hooks),
            "repo state should win over global state"
        );

        // If repo state exists but points to managed hooks, we should not fall back to global.
        save_hook_state(
            &repo_state,
            &HooksPathState {
                previous_hooks_path: managed_git_hooks_dir().to_string_lossy().to_string(),
            },
            false,
        )
        .expect("failed to update repo state");
        assert!(
            should_forward_repo_state_first(Some(&repo)).is_none(),
            "repo state presence should suppress global fallback"
        );
    }
}
