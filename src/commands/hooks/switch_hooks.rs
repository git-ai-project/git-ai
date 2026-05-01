use crate::authorship::virtual_attribution::{VirtualAttributions, restore_stashed_va};
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::Repository;

pub fn pre_switch_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    repository.require_pre_command_head();

    // If --merge is used, we need to capture VirtualAttributions before the switch
    // because the merge might shift lines around
    if is_merge_switch(parsed_args) && has_uncommitted_changes(repository) {
        capture_va_for_merge(parsed_args, repository, command_hooks_context);
    }
}

/// Capture VirtualAttributions before a --merge switch.
fn capture_va_for_merge(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    tracing::debug!(
        "Detected switch --merge with uncommitted changes, capturing VirtualAttributions"
    );

    let head_sha = match repository.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => {
            tracing::debug!("Failed to get HEAD for VA capture");
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
                tracing::debug!(
                    "Captured VA with {} files for switch --merge preservation",
                    va.attributions.len()
                );
                command_hooks_context.stashed_va = Some(va);
            } else {
                tracing::debug!("No attributions in working log to preserve");
            }
        }
        Err(e) => {
            tracing::debug!("Failed to build VirtualAttributions: {}", e);
        }
    }
}

pub fn post_switch_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    exit_status: std::process::ExitStatus,
    command_hooks_context: &mut CommandHooksContext,
) {
    let is_merge = is_merge_switch(parsed_args);

    // Fix #957 equivalent: `switch --merge` exits with code 1 when it produces conflict
    // markers, but we must still restore the stashed VA so attribution is not lost.
    // All other failed switches are skipped as before.
    if !exit_status.success() && !is_merge {
        tracing::debug!("Switch failed, skipping working log handling");
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

    // HEAD unchanged is always a no-op: no branch switch occurred.
    if old_head == new_head {
        tracing::debug!("HEAD unchanged after switch, no working log handling needed");
        return;
    }

    // Force switch - delete working log (changes discarded)
    if is_force_switch(parsed_args) {
        tracing::debug!(
            "Force switch detected, deleting working log for {}",
            &old_head
        );
        if let Err(e) = repository
            .storage
            .delete_working_log_for_base_commit(&old_head)
        {
            tracing::debug!(
                "Failed to delete working log for {}: {}",
                &old_head, e
            );
        }
        return;
    }

    // --merge switch - restore VirtualAttributions (lines may have shifted).
    // In wrapper mode the VA is captured by pre_switch_hook into stashed_va.
    // In daemon mode (where hooks run as separate processes), stashed_va is None, so
    // we rebuild the pre-switch VA directly from the working log for old_head, which
    // is still intact at this point.
    //
    // If switch --merge produced conflict markers (exit code 1), skip the fallback
    // since those files contain conflict markers which would corrupt byte-level offsets.
    if is_merge {
        let human_author = get_commit_default_author(repository, &parsed_args.command_args);
        let stashed_va = command_hooks_context.stashed_va.take().or_else(|| {
            if !exit_status.success() {
                return None;
            }
            VirtualAttributions::from_just_working_log(
                repository.clone(),
                old_head.clone(),
                Some(human_author),
            )
            .ok()
            .filter(|va| !va.attributions.is_empty())
        });

        if let Some(stashed_va) = stashed_va {
            tracing::debug!("Restoring VA after switch --merge");
            if let Err(e) = repository
                .storage
                .delete_working_log_for_base_commit(&old_head)
            {
                tracing::debug!(
                    "Failed to delete working log for {}: {}",
                    &old_head, e
                );
            }
            restore_stashed_va(repository, &old_head, &new_head, stashed_va);
            return;
        }
        tracing::debug!(
            "switch --merge: no VA to restore, falling through to working log migration"
        );
        // Fall through to rename
    }

    // Normal branch switch - migrate working log
    tracing::debug!("Switch changed HEAD: {} -> {}", &old_head, &new_head);
    if let Err(e) = repository.storage.rename_working_log(&old_head, &new_head) {
        tracing::debug!(
            "Failed to rename working log {} -> {}: {}",
            &old_head, &new_head, e
        );
    }
}

/// Check if switch uses force flag (--discard-changes, -f, --force).
fn is_force_switch(parsed_args: &ParsedGitInvocation) -> bool {
    parsed_args
        .command_args
        .iter()
        .any(|arg| arg == "-f" || arg == "--force" || arg == "--discard-changes")
}

/// Check if switch uses --merge flag that merges local changes.
fn is_merge_switch(parsed_args: &ParsedGitInvocation) -> bool {
    parsed_args.has_command_flag("--merge") || parsed_args.has_command_flag("-m")
}

/// Check if the working directory has uncommitted changes.
fn has_uncommitted_changes(repository: &Repository) -> bool {
    match repository.get_staged_and_unstaged_filenames() {
        Ok(filenames) => !filenames.is_empty(),
        Err(_) => false,
    }
}
