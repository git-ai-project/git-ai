use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::commands::hooks::plumbing_rewrite_hooks::apply_wrapper_plumbing_rewrite_if_possible;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::Repository;
use crate::utils::debug_log;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedUpdateRefCommand {
    ref_name: String,
}

pub fn pre_update_ref_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    _context: &mut CommandHooksContext,
) {
    clear_pre_update_ref_state(repository);

    let Some(command) = parse_simple_update_ref(parsed_args) else {
        return;
    };
    if !should_track_ref_update(&command.ref_name) {
        return;
    }

    let current_head_ref = repository
        .head()
        .ok()
        .and_then(|head| head.name().map(|name| name.to_string()));
    let affects_checked_out_branch = command.ref_name == "HEAD"
        || current_head_ref.as_deref() == Some(command.ref_name.as_str());

    repository.pre_update_ref_refname = Some(command.ref_name.clone());
    repository.pre_update_ref_old_target = resolve_ref_target(repository, &command.ref_name);
    repository.pre_update_ref_affects_checked_out_branch = Some(affects_checked_out_branch);
}

pub fn post_update_ref_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    exit_status: std::process::ExitStatus,
    _context: &mut CommandHooksContext,
) {
    if !exit_status.success() {
        clear_pre_update_ref_state(repository);
        return;
    }

    let Some(ref_name) = repository.pre_update_ref_refname.clone() else {
        clear_pre_update_ref_state(repository);
        return;
    };
    let old_target = repository.pre_update_ref_old_target.clone();
    let affects_checked_out_branch = repository
        .pre_update_ref_affects_checked_out_branch
        .unwrap_or(false);
    clear_pre_update_ref_state(repository);

    let Some(old_target) = old_target else {
        return;
    };

    let Some(new_target) = resolve_ref_target(repository, &ref_name) else {
        return;
    };

    if old_target == new_target {
        return;
    }

    if is_ancestor(repository, &old_target, &new_target) {
        if affects_checked_out_branch {
            let _ = repository
                .storage
                .rename_working_log(&old_target, &new_target);
        }
        return;
    }

    if is_ancestor(repository, &new_target, &old_target) {
        debug_log(&format!(
            "Skipping wrapper update-ref rewind handling for {}: {} -> {}",
            ref_name, old_target, new_target
        ));
        return;
    }

    let commit_author = get_commit_default_author(repository, &parsed_args.command_args);
    if !apply_wrapper_plumbing_rewrite_if_possible(
        repository,
        &old_target,
        &new_target,
        &commit_author,
        true,
    ) {
        debug_log(&format!(
            "Skipping wrapper update-ref rewrite handling for {}: could not derive safe mappings",
            ref_name
        ));
    }
}

fn clear_pre_update_ref_state(repository: &mut Repository) {
    repository.pre_update_ref_refname = None;
    repository.pre_update_ref_old_target = None;
    repository.pre_update_ref_affects_checked_out_branch = None;
}

fn parse_simple_update_ref(parsed_args: &ParsedGitInvocation) -> Option<ParsedUpdateRefCommand> {
    if parsed_args.command.as_deref() != Some("update-ref") {
        return None;
    }

    let args = &parsed_args.command_args;
    let mut positionals = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--stdin" | "--batch-updates" | "-d" | "--delete" => return None,
            "-m" | "--message" => {
                if i + 1 >= args.len() {
                    return None;
                }
                i += 2;
                continue;
            }
            "--create-reflog" | "--no-deref" => {
                i += 1;
                continue;
            }
            _ if arg.starts_with("--message=") => {
                i += 1;
                continue;
            }
            _ if arg.starts_with('-') => return None,
            _ => {
                positionals.push(arg.clone());
                i += 1;
            }
        }
    }

    match positionals.as_slice() {
        [ref_name, _new_oid] => Some(ParsedUpdateRefCommand {
            ref_name: ref_name.clone(),
        }),
        [ref_name, _new_oid, _old_oid] => Some(ParsedUpdateRefCommand {
            ref_name: ref_name.clone(),
        }),
        _ => None,
    }
}

fn should_track_ref_update(ref_name: &str) -> bool {
    ref_name == "HEAD" || ref_name.starts_with("refs/heads/")
}

fn resolve_ref_target(repository: &Repository, ref_name: &str) -> Option<String> {
    repository
        .revparse_single(ref_name)
        .and_then(|obj| obj.peel_to_commit())
        .map(|commit| commit.id())
        .ok()
}

fn is_ancestor(repository: &Repository, ancestor: &str, descendant: &str) -> bool {
    let mut args = repository.global_args_for_exec();
    args.push("merge-base".to_string());
    args.push("--is-ancestor".to_string());
    args.push(ancestor.to_string());
    args.push(descendant.to_string());
    crate::git::repository::exec_git(&args).is_ok()
}

#[cfg(test)]
mod tests {
    use super::parse_simple_update_ref;
    use crate::git::cli_parser::parse_git_cli_args;

    #[test]
    fn parses_simple_update_ref() {
        let parsed = parse_git_cli_args(&[
            "update-ref".to_string(),
            "refs/heads/topic".to_string(),
            "abc123".to_string(),
        ]);
        let command = parse_simple_update_ref(&parsed).expect("should parse");
        assert_eq!(command.ref_name, "refs/heads/topic");
    }

    #[test]
    fn rejects_update_ref_stdin_mode() {
        let parsed = parse_git_cli_args(&["update-ref".to_string(), "--stdin".to_string()]);
        assert!(parse_simple_update_ref(&parsed).is_none());
    }
}
