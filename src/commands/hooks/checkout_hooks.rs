use crate::authorship::virtual_attribution::{VirtualAttributions, restore_stashed_va};
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::Repository;
use crate::utils::debug_log;

pub fn pre_checkout_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    repository.require_pre_command_head();

    // If --merge is used, we need to capture VirtualAttributions before the checkout
    // because the merge might shift lines around
    if is_merge_checkout(parsed_args) && has_uncommitted_changes(repository) {
        capture_va_for_merge(parsed_args, repository, command_hooks_context);
    }
}

/// Check if checkout uses force flag (-f, --force) that discards local changes.
fn is_force_checkout(parsed_args: &ParsedGitInvocation) -> bool {
    parsed_args
        .command_args
        .iter()
        .any(|arg| arg == "-f" || arg == "--force")
}

/// Check if checkout uses --merge flag that merges local changes.
fn is_merge_checkout(parsed_args: &ParsedGitInvocation) -> bool {
    parsed_args.has_command_flag("--merge") || parsed_args.has_command_flag("-m")
}

/// Check if the working directory has uncommitted changes.
fn has_uncommitted_changes(repository: &Repository) -> bool {
    match repository.get_staged_and_unstaged_filenames() {
        Ok(filenames) => !filenames.is_empty(),
        Err(_) => false,
    }
}

/// Capture VirtualAttributions before a --merge checkout.
fn capture_va_for_merge(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    debug_log("Detected checkout --merge with uncommitted changes, capturing VirtualAttributions");

    let head_sha = match repository.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => {
            debug_log("Failed to get HEAD for VA capture");
            return;
        }
    };

    let human_author = get_commit_default_author(repository, &parsed_args.command_args);
    match VirtualAttributions::from_just_working_log(
        repository.clone(),
        head_sha.clone(),
        Some(human_author),
    ) {
        Ok(va) => {
            if !va.attributions.is_empty() {
                debug_log(&format!(
                    "Captured VA with {} files for checkout --merge preservation",
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

pub fn post_checkout_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    exit_status: std::process::ExitStatus,
    command_hooks_context: &mut CommandHooksContext,
) {
    if !exit_status.success() {
        debug_log("Checkout failed, skipping working log handling");
        return;
    }

    let old_head = match &repository.pre_command_base_commit {
        Some(sha) => sha.clone(),
        None => return,
    };

    let new_head = match repository.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => return,
    };

    let pathspecs = parsed_args.pathspecs();

    // Case 1: Pathspec checkout (git checkout branch -- file.txt)
    // HEAD unchanged, specific files reverted - remove their attributions
    if !pathspecs.is_empty() {
        debug_log(&format!(
            "Pathspec checkout detected, removing attributions for: {:?}",
            pathspecs
        ));
        remove_attributions_for_pathspecs(repository, &old_head, &pathspecs);
        return;
    }

    // Case 2: HEAD unchanged (e.g., checkout current branch)
    if old_head == new_head {
        debug_log("HEAD unchanged after checkout, no working log handling needed");
        return;
    }

    // Case 3: Force checkout - delete working log (changes discarded)
    if is_force_checkout(parsed_args) {
        debug_log(&format!(
            "Force checkout detected, deleting working log for {}",
            &old_head
        ));
        let _ = repository
            .storage
            .delete_working_log_for_base_commit(&old_head);
        return;
    }

    // Case 4: --merge checkout - restore VirtualAttributions (lines may have shifted)
    if let Some(stashed_va) = command_hooks_context.stashed_va.take() {
        debug_log("Restoring VA after checkout --merge");
        let _ = repository
            .storage
            .delete_working_log_for_base_commit(&old_head);
        restore_stashed_va(repository, &old_head, &new_head, stashed_va);
        return;
    }

    // Case 5: Normal branch checkout - migrate working log
    debug_log(&format!(
        "Checkout changed HEAD: {} -> {}",
        &old_head, &new_head
    ));
    let _ = repository.storage.rename_working_log(&old_head, &new_head);
}

/// Remove attributions for specific files from working log (pathspec checkout case).
fn remove_attributions_for_pathspecs(repository: &Repository, head: &str, pathspecs: &[String]) {
    let working_log = repository.storage.working_log_for_base_commit(head);

    // Filter INITIAL attributions
    let initial = working_log.read_initial_attributions();
    if !initial.files.is_empty() {
        let filtered_files = initial
            .files
            .into_iter()
            .filter(|(file, _)| !matches_any_pathspec(file, pathspecs))
            .collect();
        let _ = working_log.write_initial_attributions(filtered_files, initial.prompts);
    }

    // Filter checkpoints
    if let Ok(checkpoints) = working_log.read_all_checkpoints() {
        let filtered: Vec<_> = checkpoints
            .into_iter()
            .map(|mut cp| {
                cp.entries
                    .retain(|entry| !matches_any_pathspec(&entry.file, pathspecs));
                cp
            })
            .filter(|cp| !cp.entries.is_empty())
            .collect();
        let _ = working_log.write_all_checkpoints(&filtered);
    }
}

fn matches_any_pathspec(file: &str, pathspecs: &[String]) -> bool {
    pathspecs.iter().any(|p| {
        file == p
            || (p.ends_with('/') && file.starts_with(p))
            || file.starts_with(&format!("{}/", p))
    })
}
