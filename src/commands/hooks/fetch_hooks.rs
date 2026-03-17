use crate::authorship::virtual_attribution::{VirtualAttributions, restore_stashed_va};
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::commands::hooks::rebase_hooks::build_rebase_commit_mappings;
use crate::commands::upgrade;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::repository::{Repository, exec_git, find_repository};
use crate::git::rewrite_log::RewriteLogEvent;
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
    parsed_args: &ParsedGitInvocation,
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
    }

    // Check for fast-forward pull and rename working log if applicable
    if was_fast_forward_pull(repository, &new_head) {
        debug_log(&format!(
            "Fast-forward detected: {} -> {}",
            old_head, new_head
        ));
        let _ = repository.storage.rename_working_log(&old_head, &new_head);
        return;
    }

    // Handle committed authorship rewriting for pull --rebase
    let config = get_pull_rebase_autostash_config(parsed_args, repository);
    if config.is_rebase {
        process_completed_pull_rebase(repository, &old_head, &new_head);
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

/// Rewrite authorship for committed local changes that were rebased by `git pull --rebase`.
/// Uses the same commit-mapping and rewrite logic as `rebase_hooks::process_completed_rebase`.
fn process_completed_pull_rebase(repository: &mut Repository, original_head: &str, new_head: &str) {
    debug_log(&format!(
        "Processing pull --rebase authorship: {} -> {}",
        original_head, new_head
    ));

    let onto_head = resolve_pull_rebase_onto_head(repository);
    let (original_commits, new_commits) = match build_rebase_commit_mappings(
        repository,
        original_head,
        new_head,
        onto_head.as_deref(),
    ) {
        Ok(mappings) => {
            debug_log(&format!(
                "Pull rebase mappings: {} original -> {} new commits",
                mappings.0.len(),
                mappings.1.len()
            ));
            mappings
        }
        Err(e) => {
            debug_log(&format!("Failed to build pull rebase mappings: {}", e));
            return;
        }
    };

    if original_commits.is_empty() {
        debug_log("No committed changes to rewrite authorship for after pull --rebase");
        return;
    }
    if new_commits.is_empty() {
        debug_log("No newly rebased commits to rewrite authorship for after pull --rebase");
        return;
    }

    let rebase_event =
        RewriteLogEvent::rebase_complete(crate::git::rewrite_log::RebaseCompleteEvent::new(
            original_head.to_string(),
            new_head.to_string(),
            false, // pull --rebase is not interactive
            original_commits,
            new_commits,
        ));

    let commit_author = get_commit_default_author(repository, &[]);
    repository.handle_rewrite_log_event(
        rebase_event,
        commit_author,
        false, // don't suppress output
        true,  // save to log
    );

    debug_log("Pull --rebase authorship rewrite complete");
}

fn resolve_pull_rebase_onto_head(repository: &Repository) -> Option<String> {
    repository
        .revparse_single("@{upstream}")
        .and_then(|obj| obj.peel_to_commit())
        .map(|commit| commit.id())
        .ok()
}
