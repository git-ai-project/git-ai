use crate::{
    commands::hooks::commit_hooks::get_commit_default_author,
    git::{
        cli_parser::{ParsedGitInvocation, is_dry_run},
        repository::Repository,
        rewrite_log::{MergeSquashEvent, RewriteLogEvent},
    },
};

pub fn post_merge_hook(
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    if parsed_args.has_command_flag("--squash")
        && exit_status.success()
        && !is_dry_run(&parsed_args.command_args)
    {
        let base_branch = repository.head().unwrap().name().unwrap().to_string();
        let base_head = repository.head().unwrap().target().unwrap().to_string();

        let commit_author = get_commit_default_author(repository, &parsed_args.command_args);

        let source_branch = parsed_args.pos_command(0).unwrap();

        let source_head_sha = match repository
            .revparse_single(source_branch.as_str())
            .and_then(|obj| obj.peel_to_commit())
        {
            Ok(commit) => commit.id(),
            Err(_) => {
                // If we can't resolve the branch, skip logging this event
                return;
            }
        };

        repository.handle_rewrite_log_event(
            RewriteLogEvent::merge_squash(MergeSquashEvent::new(
                source_branch.clone(),
                source_head_sha,
                base_branch,
                base_head,
            )),
            commit_author,
            false,
            true,
        );
    }
}
