use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::upgrade;
use crate::git::cli_parser::{is_dry_run, ParsedGitInvocation};
use crate::git::repository::{find_repository, Repository};
use crate::git::sync_authorship::push_authorship_notes;

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
        tracing::debug!("started pushing authorship notes to remote: {}", remote);
        // Clone what we need for the background thread
        let global_args = repository.global_args_for_exec();

        // Spawn background thread to push authorship notes in parallel with main push
        Some(std::thread::spawn(move || {
            // Recreate repository in the background thread
            if let Ok(repo) = find_repository(&global_args) {
                if let Err(e) = push_authorship_notes(&repo, &remote) {
                    tracing::debug!("authorship push failed: {}", e);
                }
            } else {
                tracing::debug!("failed to open repository for authorship push");
            }
        }))
    } else {
        // No remotes configured; skip silently
        tracing::debug!("no remotes found for authorship push; skipping");
        None
    }
}

pub fn run_pre_push_hook_managed(parsed_args: &ParsedGitInvocation, repository: &Repository) {
    upgrade::maybe_schedule_background_update_check();

    if should_skip_authorship_push(&parsed_args.command_args) {
        return;
    }

    let Some(remote) = resolve_push_remote(parsed_args, repository) else {
        tracing::debug!("no remotes found for authorship push; skipping");
        return;
    };

    tracing::debug!("started pushing authorship notes to remote: {}", remote);

    if let Err(e) = push_authorship_notes(repository, &remote) {
        tracing::debug!("authorship push failed: {}", e);
    }
}

pub fn capture_tracker_state(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> (
    Option<std::collections::HashMap<String, String>>,
    Option<String>,
) {
    let remote = resolve_push_remote(parsed_args, repository);

    let refs = std::process::Command::new("git")
        .args(&[
            "-C",
            &repository.path().to_string_lossy(),
            "for-each-ref",
            "refs/heads/",
            "--format=%(refname:short) %(objectname)",
        ])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let mut map = std::collections::HashMap::new();
                for line in text.lines() {
                    let parts: Vec<&str> = line.splitn(2, ' ').collect();
                    if parts.len() == 2 {
                        map.insert(parts[0].to_string(), parts[1].to_string());
                    }
                }
                Some(map)
            } else {
                None
            }
        });

    (refs, remote)
}

pub fn push_post_command_hook(
    repository: &Repository,
    _parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    command_hooks_context: &mut CommandHooksContext,
) {
    if let Some(handle) = command_hooks_context.push_authorship_handle.take() {
        let _ = handle.join();
    }

    if exit_status.success() {
        if let (Some(pre_push_refs), Some(remote)) = (
            command_hooks_context.tracker_pre_push_refs.take(),
            command_hooks_context.tracker_push_remote.take(),
        ) {
            let repo_path = repository.path().to_string_lossy().to_string();
            crate::commands::tracker::report_pushed_commits(&repo_path, &pre_push_refs, &remote);
        }
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
