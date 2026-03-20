use crate::commands::hooks::rebase_hooks::build_rebase_commit_mappings;
use crate::git::repository::Repository;
use crate::git::rewrite_log::{RebaseCompleteEvent, RewriteLogEvent};
use crate::utils::debug_log;

pub(crate) fn apply_wrapper_plumbing_rewrite_if_possible(
    repository: &mut Repository,
    old_head: &str,
    new_head: &str,
    commit_author: &str,
    suppress_output: bool,
) -> bool {
    if old_head == new_head {
        return false;
    }

    let Ok((original_commits, new_commits)) =
        build_rebase_commit_mappings(repository, old_head, new_head, None)
    else {
        return false;
    };

    if original_commits.is_empty() || new_commits.is_empty() {
        return false;
    }

    debug_log(&format!(
        "Applying wrapper plumbing rewrite handling: {} original commits -> {} new commits",
        original_commits.len(),
        new_commits.len()
    ));

    repository.handle_rewrite_log_event(
        RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
            old_head.to_string(),
            new_head.to_string(),
            false,
            original_commits,
            new_commits,
        )),
        commit_author.to_string(),
        suppress_output,
        true,
    );
    true
}
