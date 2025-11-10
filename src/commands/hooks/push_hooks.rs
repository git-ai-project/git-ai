use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::upgrade;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::repository::{Repository, find_repository};
use crate::git::sync_authorship::push_authorship_notes;
use crate::utils::debug_log;

pub fn push_pre_command_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<std::thread::JoinHandle<()>> {
    upgrade::maybe_schedule_background_update_check();

    // Early returns for cases where we shouldn't push authorship notes
    if is_dry_run(&parsed_args.command_args)
        || parsed_args
            .command_args
            .iter()
            .any(|a| a == "-d" || a == "--delete")
        || parsed_args.command_args.iter().any(|a| a == "--mirror")
    {
        return None;
    }

    let remotes = repository.remotes().ok();
    let remote_names: Vec<String> = remotes
        .as_ref()
        .map(|r| {
            (0..r.len())
                .filter_map(|i| r.get(i).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Push authorship refs to the appropriate remote
    let positional_remote = extract_remote_from_push_args(&parsed_args.command_args, &remote_names);

    let specified_remote = positional_remote.or_else(|| {
        parsed_args
            .command_args
            .iter()
            .find(|a| remote_names.iter().any(|r| r == *a))
            .cloned()
    });

    let remote = specified_remote
        .or_else(|| repository.upstream_remote().ok().flatten())
        .or_else(|| repository.get_default_remote().ok().flatten());

    if let Some(remote) = remote {
        debug_log(&format!(
            "started pushing authorship notes to remote: {}",
            remote
        ));
        // Clone what we need for the background thread
        let global_args = repository.global_args_for_exec();

        crate::observability::spawn_background_flush();

        // Spawn background thread to push authorship notes in parallel with main push
        Some(std::thread::spawn(move || {
            // Recreate repository in the background thread
            if let Ok(repo) = find_repository(&global_args) {
                if let Err(e) = push_authorship_notes(&repo, &remote) {
                    debug_log(&format!("authorship push failed: {}", e));
                }
            } else {
                debug_log("failed to open repository for authorship push");
            }
        }))
    } else {
        // No remotes configured; skip silently
        debug_log("no remotes found for authorship push; skipping");
        None
    }
}

pub fn push_post_command_hook(
    _repository: &Repository,
    _parsed_args: &ParsedGitInvocation,
    _exit_status: std::process::ExitStatus,
    command_hooks_context: &mut CommandHooksContext,
) {
    // Always wait for the authorship push thread to complete if it was started,
    // regardless of whether the main push succeeded or failed.
    // This ensures proper cleanup of the background thread.
    if let Some(handle) = command_hooks_context.push_authorship_handle.take() {
        let _ = handle.join();
    }
}

fn extract_remote_from_push_args(args: &[String], known_remotes: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            return args.get(i + 1).cloned();
        }
        if arg.starts_with('-') {
            if let Some((flag, value)) = is_push_option_with_inline_value(arg) {
                if flag == "--repo" {
                    return Some(value.to_string());
                }
                i += 1;
                continue;
            }

            if option_consumes_separate_value(arg.as_str()) {
                if arg == "--repo" {
                    return args.get(i + 1).cloned();
                }
                i += 2;
                continue;
            }

            i += 1;
            continue;
        }
        return Some(arg.clone());
    }

    known_remotes
        .iter()
        .find(|r| args.iter().any(|arg| arg == *r))
        .cloned()
}

fn is_push_option_with_inline_value(arg: &str) -> Option<(&str, &str)> {
    if let Some((flag, value)) = arg.split_once('=') {
        Some((flag, value))
    } else if (arg.starts_with("-C") || arg.starts_with("-c")) && arg.len() > 2 {
        // Treat -C<path> or -c<name>=<value> as inline values
        let flag = &arg[..2];
        let value = &arg[2..];
        Some((flag, value))
    } else {
        None
    }
}

fn option_consumes_separate_value(arg: &str) -> bool {
    matches!(
        arg,
        "--repo" | "--receive-pack" | "--exec" | "-o" | "--push-option" | "-c" | "-C"
    )
}
