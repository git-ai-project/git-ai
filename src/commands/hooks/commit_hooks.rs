use crate::authorship::pre_commit;
use crate::commands::git_handlers::CommandHooksContext;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::repository::Repository;
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::debug_log;

pub fn commit_pre_command_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
) -> bool {
    if is_dry_run(&parsed_args.command_args) {
        return false;
    }

    // store HEAD context for post-command hook
    repository.require_pre_command_head();

    let default_author = get_commit_default_author(&repository, &parsed_args.command_args);

    // Run pre-commit logic
    if let Err(e) = pre_commit::pre_commit(&repository, default_author.clone()) {
        if e.to_string()
            .contains("Cannot run checkpoint on bare repositories")
        {
            eprintln!(
                "Cannot run checkpoint on bare repositories (skipping git-ai pre-commit hook)"
            );
            return false;
        }
        eprintln!("Pre-commit failed: {}", e);
        std::process::exit(1);
    }
    return true;
}

pub fn commit_post_command_hook(
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    if is_dry_run(&parsed_args.command_args) {
        return;
    }

    if !exit_status.success() {
        return;
    }

    if let Some(pre_commit_hook_result) = command_hooks_context.pre_commit_hook_result {
        if !pre_commit_hook_result {
            debug_log("Skipping git-ai post-commit hook because pre-commit hook failed");
            return;
        }
    }

    let supress_output = parsed_args.has_command_flag("--porcelain")
        || parsed_args.has_command_flag("--quiet")
        || parsed_args.has_command_flag("-q")
        || parsed_args.has_command_flag("--no-status");

    let original_commit = repository.pre_command_base_commit.clone();
    let new_sha = repository.head().ok().map(|h| h.target().ok()).flatten();

    // empty repo, commit did not land
    if new_sha.is_none() {
        return;
    }

    let commit_author = get_commit_default_author(repository, &parsed_args.command_args);
    if parsed_args.has_command_flag("--amend") && original_commit.is_some() && new_sha.is_some() {
        repository.handle_rewrite_log_event(
            RewriteLogEvent::commit_amend(original_commit.unwrap(), new_sha.unwrap()),
            commit_author,
            supress_output,
            true,
        );
    } else {
        repository.handle_rewrite_log_event(
            RewriteLogEvent::commit(original_commit, new_sha.unwrap()),
            commit_author,
            supress_output,
            true,
        );
    }

    // Flush logs and metrics after commit
    crate::observability::spawn_background_flush();
}

pub fn get_commit_default_author(repo: &Repository, args: &[String]) -> String {
    // According to git commit manual, --author flag overrides all other author information
    if let Some(author_spec) = extract_author_from_args(args) {
        if let Ok(Some(resolved_author)) = repo.resolve_author_spec(&author_spec) {
            if !resolved_author.trim().is_empty() {
                return resolved_author.trim().to_string();
            }
        }
    }

    // Normal precedence when --author is not specified:
    // Name precedence: GIT_AUTHOR_NAME env > user.name config > extract from EMAIL env > "unknown"
    // Email precedence: GIT_AUTHOR_EMAIL env > user.email config > EMAIL env > None

    let mut author_name: Option<String> = None;
    let mut author_email: Option<String> = None;

    // Check GIT_AUTHOR_NAME environment variable
    if let Ok(name) = std::env::var("GIT_AUTHOR_NAME") {
        if !name.trim().is_empty() {
            author_name = Some(name.trim().to_string());
        }
    }

    // Fall back to git config user.name
    if author_name.is_none() {
        if let Ok(Some(name)) = repo.config_get_str("user.name") {
            if !name.trim().is_empty() {
                author_name = Some(name.trim().to_string());
            }
        }
    }

    // Check GIT_AUTHOR_EMAIL environment variable
    if let Ok(email) = std::env::var("GIT_AUTHOR_EMAIL") {
        if !email.trim().is_empty() {
            author_email = Some(email.trim().to_string());
        }
    }

    // Fall back to git config user.email
    if author_email.is_none() {
        if let Ok(Some(email)) = repo.config_get_str("user.email") {
            if !email.trim().is_empty() {
                author_email = Some(email.trim().to_string());
            }
        }
    }

    // Check EMAIL environment variable as fallback for both name and email
    if author_name.is_none() || author_email.is_none() {
        if let Ok(email) = std::env::var("EMAIL") {
            if !email.trim().is_empty() {
                // Extract name part from email if we don't have a name yet
                if author_name.is_none() {
                    if let Some(at_pos) = email.find('@') {
                        let name_part = &email[..at_pos];
                        if !name_part.is_empty() {
                            author_name = Some(name_part.to_string());
                        }
                    }
                }
                // Use as email if we don't have an email yet
                if author_email.is_none() {
                    author_email = Some(email.trim().to_string());
                }
            }
        }
    }

    // Format the author string based on what we have
    match (author_name, author_email) {
        (Some(name), Some(email)) => format!("{} <{}>", name, email),
        (Some(name), None) => name,
        (None, Some(email)) => email,
        (None, None) => {
            eprintln!("Warning: No author information found. Using 'unknown' as author.");
            "unknown".to_string()
        }
    }
}

fn extract_author_from_args(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        // Handle --author=<author> format
        if let Some(author_value) = arg.strip_prefix("--author=") {
            return Some(author_value.to_string());
        }

        // Handle --author <author> format (separate arguments)
        if arg == "--author" && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }

        i += 1;
    }
    None
}
