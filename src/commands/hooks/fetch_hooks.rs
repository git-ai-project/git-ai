use crate::authorship::virtual_attribution::{
    VirtualAttributions, merge_attributions_favoring_first,
};
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::commands::upgrade;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::repository::{Repository, exec_git, find_repository};
use crate::git::sync_authorship::{fetch_authorship_notes, fetch_remote_from_args};
use crate::utils::debug_log;

pub fn fetch_pull_pre_command_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<std::thread::JoinHandle<()>> {
    upgrade::maybe_schedule_background_update_check();

    // Early return for dry-run
    if is_dry_run(&parsed_args.command_args) {
        return None;
    }

    crate::observability::spawn_background_flush();

    // Extract the remote name
    let remote = match fetch_remote_from_args(repository, parsed_args) {
        Ok(remote) => remote,
        Err(_) => {
            debug_log("failed to extract remote for authorship fetch; skipping");
            return None;
        }
    };

    // Clone what we need for the background thread
    let global_args = repository.global_args_for_exec();

    // Spawn background thread to fetch authorship notes in parallel with main fetch
    Some(std::thread::spawn(move || {
        debug_log(&format!(
            "started fetching authorship notes from remote: {}",
            remote
        ));
        // Recreate repository in the background thread
        if let Ok(repo) = find_repository(&global_args) {
            if let Err(e) = fetch_authorship_notes(&repo, &remote) {
                debug_log(&format!("authorship fetch failed: {}", e));
            }
        } else {
            debug_log("failed to open repository for authorship fetch");
        }
    }))
}

/// Pre-command hook for git pull.
/// In addition to the standard fetch operations, this captures VirtualAttributions
/// when pull --rebase --autostash is detected to preserve AI authorship.
pub fn pull_pre_command_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    // Start the background authorship fetch (same as regular fetch)
    command_hooks_context.fetch_authorship_handle =
        fetch_pull_pre_command_hook(parsed_args, repository);

    // Capture HEAD before pull to detect changes
    repository.require_pre_command_head();

    // Check if this is a rebase pull with autostash (single git config call)
    let config = get_pull_rebase_autostash_config(parsed_args, repository);
    let has_changes = has_uncommitted_changes(repository);

    debug_log(&format!(
        "pull pre-hook: rebase={}, autostash={}, has_changes={}",
        config.is_rebase, config.is_autostash, has_changes
    ));

    // Only capture VA if we're in rebase+autostash mode AND have uncommitted changes
    if config.is_rebase && config.is_autostash && has_changes {
        debug_log(
            "Detected pull --rebase --autostash with uncommitted changes, capturing VirtualAttributions",
        );

        // Get current HEAD
        let head_sha = match repository.head().ok().and_then(|h| h.target().ok()) {
            Some(sha) => sha,
            None => {
                debug_log("Failed to get HEAD for VA capture");
                return;
            }
        };

        // Build VirtualAttributions from working log (fast path, no blame needed)
        let human_author = get_commit_default_author(repository, &parsed_args.command_args);
        match VirtualAttributions::from_just_working_log(
            repository.clone(),
            head_sha.clone(),
            Some(human_author),
        ) {
            Ok(va) => {
                if !va.attributions.is_empty() {
                    debug_log(&format!(
                        "Captured VA with {} files for autostash preservation",
                        va.attributions.len()
                    ));
                    command_hooks_context.stashed_va = Some(va);
                } else {
                    debug_log("No attributions in working log to preserve");
                }
            }
            Err(e) => {
                debug_log(&format!("Failed to build VirtualAttributions: {}", e));
            }
        }
    }
}

pub fn fetch_pull_post_command_hook(
    _repository: &Repository,
    _parsed_args: &ParsedGitInvocation,
    _exit_status: std::process::ExitStatus,
    command_hooks_context: &mut CommandHooksContext,
) {
    // Always wait for the authorship fetch thread to complete if it was started,
    // regardless of whether the main fetch/pull succeeded or failed.
    // This ensures proper cleanup of the background thread.
    if let Some(handle) = command_hooks_context.fetch_authorship_handle.take() {
        let _ = handle.join();
    }
}

/// Post-command hook for git pull.
/// Handles two scenarios:
/// 1. Restores AI attributions after a pull --rebase --autostash operation.
/// 2. Renames working log for fast-forward pulls to preserve attributions.
pub fn pull_post_command_hook(
    repository: &mut Repository,
    _parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    command_hooks_context: &mut CommandHooksContext,
) {
    // Wait for authorship fetch thread
    if let Some(handle) = command_hooks_context.fetch_authorship_handle.take() {
        let _ = handle.join();
    }

    if !exit_status.success() {
        debug_log("Pull failed, skipping post-pull authorship restoration");
        return;
    }

    // Get old HEAD from pre-command capture
    let old_head = match &repository.pre_command_base_commit {
        Some(sha) => sha.clone(),
        None => return,
    };

    // Get new HEAD
    let new_head = match repository.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => return,
    };

    if old_head == new_head {
        debug_log("HEAD unchanged, skipping post-pull authorship handling");
        return;
    }

    // Check if we have a stashed VA to restore (from pull --rebase --autostash)
    if let Some(stashed_va) = command_hooks_context.stashed_va.take() {
        restore_stashed_va(repository, &old_head, &new_head, stashed_va);
        return;
    }

    // No stashed VA - check for fast-forward pull and rename working log if applicable
    if was_fast_forward_pull(repository, &new_head) {
        debug_log(&format!(
            "Fast-forward detected: {} -> {}",
            old_head, new_head
        ));
        let _ = repository.storage.rename_working_log(&old_head, &new_head);
    }
}

/// Restore stashed VirtualAttributions after a pull --rebase --autostash operation.
fn restore_stashed_va(
    repository: &mut Repository,
    old_head: &str,
    new_head: &str,
    stashed_va: VirtualAttributions,
) {
    debug_log(&format!(
        "Restoring stashed VA after pull --rebase --autostash: {} -> {}",
        old_head, new_head
    ));

    // Get the files that were in the stashed VA
    let stashed_files: Vec<String> = stashed_va.files();

    if stashed_files.is_empty() {
        debug_log("Stashed VA has no files, nothing to restore");
        return;
    }

    // Get current working directory file contents (final state after autostash apply)
    let mut working_files = std::collections::HashMap::new();
    if let Ok(workdir) = repository.workdir() {
        for file_path in &stashed_files {
            let abs_path = workdir.join(file_path);
            if abs_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&abs_path) {
                    working_files.insert(file_path.clone(), content);
                }
            }
        }
    }

    if working_files.is_empty() {
        debug_log("No working files to restore attributions for");
        return;
    }

    // Build a VA for the new HEAD state (if there are any existing attributions)
    let new_va = match VirtualAttributions::from_just_working_log(
        repository.clone(),
        new_head.to_string(),
        None,
    ) {
        Ok(va) => va,
        Err(e) => {
            debug_log(&format!("Failed to build new VA: {}, using empty", e));
            VirtualAttributions::new(
                repository.clone(),
                new_head.to_string(),
                std::collections::HashMap::new(),
                std::collections::HashMap::new(),
                0,
            )
        }
    };

    // Merge VAs, favoring the stashed VA (our original work)
    let merged_va = match merge_attributions_favoring_first(stashed_va, new_va, working_files) {
        Ok(va) => va,
        Err(e) => {
            debug_log(&format!("Failed to merge VirtualAttributions: {}", e));
            return;
        }
    };

    // Convert merged VA to INITIAL attributions for the new HEAD
    // Since these are uncommitted changes, we use the same SHA for parent and commit
    // to get all attributions into the INITIAL file (not the authorship log)
    let (_authorship_log, initial_attributions) = match merged_va
        .to_authorship_log_and_initial_working_log(repository, new_head, new_head, None)
    {
        Ok(result) => result,
        Err(e) => {
            debug_log(&format!("Failed to convert VA to INITIAL: {}", e));
            return;
        }
    };

    // Write INITIAL attributions to working log for new HEAD
    if !initial_attributions.files.is_empty() || !initial_attributions.prompts.is_empty() {
        let working_log = repository.storage.working_log_for_base_commit(new_head);
        if let Err(e) = working_log
            .write_initial_attributions(initial_attributions.files, initial_attributions.prompts)
        {
            debug_log(&format!("Failed to write INITIAL attributions: {}", e));
            return;
        }

        debug_log(&format!(
            "✓ Restored AI attributions to INITIAL for new HEAD {}",
            &new_head[..8]
        ));
    }
}

/// Check if the most recent reflog entry indicates a fast-forward pull operation.
/// Uses format "%H %gs" to get both the commit SHA and the reflog subject.
/// Verifies:
/// 1. The reflog SHA matches the expected new HEAD (confirms we have the right entry)
/// 2. The subject starts with "pull" (confirms it was a pull operation)
/// 3. The subject ends with ": Fast-forward" (confirms it was a fast-forward)
fn was_fast_forward_pull(repository: &Repository, expected_new_head: &str) -> bool {
    let mut args = repository.global_args_for_exec();
    args.extend(
        ["reflog", "-1", "--format=%H %gs"]
            .iter()
            .map(|s| s.to_string()),
    );

    match exec_git(&args) {
        Ok(output) => {
            let output_str = String::from_utf8_lossy(&output.stdout);
            let output_str = output_str.trim();

            // Format: "<sha> <subject>"
            // Example: "1f9a5dc45612afcbef17e9d07441d9b57c7bb5d0 pull: Fast-forward"
            let Some((sha, subject)) = output_str.split_once(' ') else {
                return false;
            };

            // Verify the SHA matches our expected new HEAD
            if sha != expected_new_head {
                debug_log(&format!(
                    "Reflog SHA {} doesn't match expected HEAD {}",
                    sha, expected_new_head
                ));
                return false;
            }

            // Must be a pull command that resulted in fast-forward
            subject.starts_with("pull") && subject.ends_with(": Fast-forward")
        }
        Err(_) => false,
    }
}

/// Result of checking pull rebase and autostash settings
struct PullRebaseAutostashConfig {
    is_rebase: bool,
    is_autostash: bool,
}

/// Check if a pull operation will use rebase and autostash based on config and CLI flags.
/// CLI flags override config settings. Uses a single git config call to minimize overhead.
fn get_pull_rebase_autostash_config(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> PullRebaseAutostashConfig {
    // Check CLI flags first - they take precedence and don't require git calls
    let rebase_from_cli = if parsed_args.has_command_flag("--no-rebase") {
        Some(false)
    } else if parsed_args.has_command_flag("--rebase") || parsed_args.has_command_flag("-r") {
        Some(true)
    } else {
        None
    };

    let autostash_from_cli = if parsed_args.has_command_flag("--no-autostash") {
        Some(false)
    } else if parsed_args.has_command_flag("--autostash") {
        Some(true)
    } else {
        None
    };

    // If both are determined by CLI flags, no need to check config
    if let (Some(is_rebase), Some(is_autostash)) = (rebase_from_cli, autostash_from_cli) {
        return PullRebaseAutostashConfig {
            is_rebase,
            is_autostash,
        };
    }

    // Get relevant config values in a single git call
    // Pattern matches: pull.rebase, rebase.autoStash
    let config = repository
        .config_get_regexp(r"^(pull\.rebase|rebase\.autoStash)$")
        .unwrap_or_default();

    // Determine rebase setting
    let is_rebase = rebase_from_cli.unwrap_or_else(|| {
        // Check git config: pull.rebase can be true, false, merges, interactive, or preserve
        // Any value other than "false" means rebase mode is enabled
        config
            .get("pull.rebase")
            .map(|v| v.to_lowercase() != "false")
            .unwrap_or(false)
    });

    // Determine autostash setting
    let is_autostash = autostash_from_cli.unwrap_or_else(|| {
        // Check git config: rebase.autoStash (used when rebasing)
        config
            .get("rebase.autoStash")
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false)
    });

    PullRebaseAutostashConfig {
        is_rebase,
        is_autostash,
    }
}

/// Check if the working directory has uncommitted changes that would trigger autostash.
fn has_uncommitted_changes(repository: &Repository) -> bool {
    // Check if there are any staged or unstaged changes
    match repository.get_staged_and_unstaged_filenames() {
        Ok(filenames) => !filenames.is_empty(),
        Err(_) => false,
    }
}
