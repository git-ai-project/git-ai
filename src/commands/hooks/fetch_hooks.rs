use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::upgrade;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::repository::{Repository, find_repository};
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
