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
    if should_skip_authorship_push(&parsed_args.command_args) {
        return None;
    }
    let remote = resolve_push_remote(parsed_args, repository);

    if let Some(remote) = remote {
        debug_log(&format!(
            "started pushing authorship notes to remote: {}",
            remote
        ));
        // Clone what we need for the background thread
        let global_args = repository.global_args_for_exec();

        crate::observability::spawn_background_flush();

        // Spawn CAS flush if prompt_storage is "default" (CAS upload mode)
        if crate::config::Config::get().prompt_storage() == "default" {
            crate::commands::flush_cas::spawn_background_cas_flush();
        }

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

pub fn run_pre_push_hook_managed(parsed_args: &ParsedGitInvocation, repository: &Repository) {
    upgrade::maybe_schedule_background_update_check();

    if should_skip_authorship_push(&parsed_args.command_args) {
        return;
    }

    let Some(remote) = resolve_push_remote(parsed_args, repository) else {
        debug_log("no remotes found for authorship push; skipping");
        return;
    };

    debug_log(&format!(
        "started pushing authorship notes to remote: {}",
        remote
    ));

    crate::observability::spawn_background_flush();

    // Spawn CAS flush if prompt_storage is "default" (CAS upload mode)
    if crate::config::Config::get().prompt_storage() == "default" {
        crate::commands::flush_cas::spawn_background_cas_flush();
    }

    if let Err(e) = push_authorship_notes(repository, &remote) {
        debug_log(&format!("authorship push failed: {}", e));
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

fn should_skip_authorship_push(command_args: &[String]) -> bool {
    is_dry_run(command_args)
        || command_args.iter().any(|a| a == "-d" || a == "--delete")
        || command_args.iter().any(|a| a == "--mirror")
}

fn resolve_push_remote(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<String> {
    let remotes = repository.remotes().ok();
    let remote_names: Vec<String> = remotes
        .as_ref()
        .map(|r| {
            (0..r.len())
                .filter_map(|i| r.get(i).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let upstream_remote = repository.upstream_remote().ok().flatten();
    let default_remote = repository.get_default_remote().ok().flatten();

    resolve_push_remote_from_parts(
        &parsed_args.command_args,
        &remote_names,
        upstream_remote,
        default_remote,
    )
}

fn resolve_push_remote_from_parts(
    command_args: &[String],
    known_remotes: &[String],
    upstream_remote: Option<String>,
    default_remote: Option<String>,
) -> Option<String> {
    let positional_remote = extract_remote_from_push_args(command_args, known_remotes);

    let specified_remote = positional_remote.or_else(|| {
        command_args
            .iter()
            .find(|arg| known_remotes.iter().any(|remote| remote == *arg))
            .cloned()
    });

    specified_remote.or(upstream_remote).or(default_remote)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn skip_authorship_push_when_dry_run() {
        assert!(should_skip_authorship_push(&strings(&["--dry-run"])));
    }

    #[test]
    fn skip_authorship_push_when_delete() {
        assert!(should_skip_authorship_push(&strings(&["--delete"])));
        assert!(should_skip_authorship_push(&strings(&["-d"])));
    }

    #[test]
    fn skip_authorship_push_when_mirror() {
        assert!(should_skip_authorship_push(&strings(&["--mirror"])));
    }

    #[test]
    fn resolve_push_remote_prefers_positional_remote() {
        let args = strings(&["origin", "main"]);
        let remote = resolve_push_remote_from_parts(
            &args,
            &strings(&["origin", "upstream"]),
            Some("upstream".to_string()),
            Some("origin".to_string()),
        );
        assert_eq!(remote.as_deref(), Some("origin"));
    }

    #[test]
    fn resolve_push_remote_prefers_repo_flag() {
        let args = strings(&["--repo", "upstream", "HEAD"]);
        let remote = resolve_push_remote_from_parts(
            &args,
            &strings(&["origin", "upstream"]),
            Some("origin".to_string()),
            None,
        );
        assert_eq!(remote.as_deref(), Some("upstream"));
    }

    #[test]
    fn resolve_push_remote_falls_back_to_upstream_then_default() {
        let args = Vec::<String>::new();
        let with_upstream = resolve_push_remote_from_parts(
            &args,
            &strings(&["origin"]),
            Some("upstream".to_string()),
            Some("origin".to_string()),
        );
        assert_eq!(with_upstream.as_deref(), Some("upstream"));

        let with_default = resolve_push_remote_from_parts(
            &args,
            &strings(&["origin"]),
            None,
            Some("origin".to_string()),
        );
        assert_eq!(with_default.as_deref(), Some("origin"));
    }
}
