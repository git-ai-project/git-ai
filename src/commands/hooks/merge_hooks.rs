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
        let head_ref = match repository.head() {
            Ok(h) => h,
            Err(_) => return,
        };
        let base_branch = match head_ref.name() {
            Some(n) => n.to_string(),
            None => return,
        };
        let base_head = match head_ref.target() {
            Ok(t) => t.to_string(),
            Err(_) => return,
        };

        let commit_author = get_commit_default_author(repository, &parsed_args.command_args);

        let Some(source_branch) = parsed_args.pos_command(0) else {
            return;
        };

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
        let staged_file_blobs = match repository.get_all_staged_file_blob_oids() {
            Ok(staged_file_blobs) => staged_file_blobs,
            Err(error) => {
                tracing::debug!("Failed to snapshot merge --squash staged blobs: {}", error);
                return;
            }
        };

        repository.handle_rewrite_log_event(
            RewriteLogEvent::merge_squash(MergeSquashEvent::new(
                source_branch.clone(),
                source_head_sha,
                base_branch,
                base_head,
                staged_file_blobs,
            )),
            commit_author,
            false,
            true,
        );
    }
}
