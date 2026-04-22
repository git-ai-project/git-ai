use crate::authorship::rebase_authorship::walk_commits_to_base;
use crate::git::repository::Repository;
use crate::git::rewrite_log::RewriteLogEvent;

fn original_equivalent_for_rewritten_commit(
    repository: &Repository,
    rewritten_commit: &str,
) -> Option<String> {
    let events = repository.storage.read_rewrite_events().ok()?;
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { rebase_complete } => {
                if let Some(index) = rebase_complete
                    .new_commits
                    .iter()
                    .position(|commit| commit == rewritten_commit)
                {
                    return rebase_complete.original_commits.get(index).cloned();
                }
            }
            RewriteLogEvent::CherryPickComplete {
                cherry_pick_complete,
            } => {
                if let Some(index) = cherry_pick_complete
                    .new_commits
                    .iter()
                    .position(|commit| commit == rewritten_commit)
                {
                    return cherry_pick_complete.source_commits.get(index).cloned();
                }
            }
            RewriteLogEvent::CommitAmend { commit_amend }
                if commit_amend.amended_commit_sha == rewritten_commit =>
            {
                return Some(commit_amend.original_commit);
            }
            _ => {}
        }
    }
    None
}

pub fn build_rebase_commit_mappings(
    repository: &Repository,
    original_head: &str,
    new_head: &str,
    onto_head: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), crate::error::GitAiError> {
    if let Some(onto_head) = onto_head
        && !crate::git::repo_state::is_valid_git_oid(onto_head)
    {
        return Err(crate::error::GitAiError::Generic(format!(
            "rebase mapping expected resolved onto oid, got '{}'",
            onto_head
        )));
    }

    // Get commits from new_head and original_head
    let new_head_commit = repository.find_commit(new_head.to_string())?;
    let original_head_commit = repository.find_commit(original_head.to_string())?;

    // Find merge base between original and new
    let merge_base = repository.merge_base(original_head_commit.id(), new_head_commit.id())?;

    let original_base = onto_head
        .and_then(|onto| original_equivalent_for_rewritten_commit(repository, onto))
        .filter(|mapped| mapped != original_head && is_ancestor(repository, mapped, original_head))
        .unwrap_or_else(|| merge_base.clone());

    // Walk from original_head to the original-side lower bound to get the commits that were rebased.
    let mut original_commits = walk_commits_to_base(repository, original_head, &original_base)?;
    original_commits.reverse();

    // If there were no original commits, there is nothing to rewrite.
    // Avoid walking potentially large parts of new history.
    if original_commits.is_empty() {
        tracing::debug!(
            "Commit mapping: 0 original -> 0 new (merge_base: {}, original_base: {})",
            merge_base,
            original_base
        );
        return Ok((original_commits, Vec::new()));
    }

    // Prefer the rebase target (onto) as the lower bound for new commits. This prevents
    // skipped/no-op rebases from sweeping unrelated target-branch history.
    // When onto_head == merge_base the caller doesn't have a real onto (e.g. daemon
    // fallback computes merge_base and passes it as onto).  Treat that the same as
    // None to avoid sweeping in target-branch commits via the ancestry-path walk.
    let validated_onto = onto_head
        .filter(|onto| *onto != merge_base)
        .filter(|onto| is_ancestor(repository, onto, new_head));
    let new_commits_base = validated_onto.unwrap_or(merge_base.as_str());

    let mut new_commits = if validated_onto.is_some() {
        // onto_head is available, valid, and distinct from merge_base — use the
        // full ancestry-path walk so --rebase-merges topologies are preserved.
        walk_commits_to_base(repository, new_head, new_commits_base)?
    } else {
        // onto_head is unavailable, equals merge_base (daemon fallback), or
        // invalid.  The range merge_base..new_head can include target-branch
        // commits (including merge commits) that were never part of the rebase.
        // Use --first-parent capped at original_commits.len() to walk only the
        // rebased tip of the branch.
        walk_first_parent_commits(
            repository,
            new_head,
            new_commits_base,
            original_commits.len(),
        )?
    };

    // Reverse so they're in chronological order (oldest first)
    new_commits.reverse();

    tracing::debug!(
        "Commit mapping: {} original -> {} new (merge_base: {}, original_base: {}, new_base: {})",
        original_commits.len(),
        new_commits.len(),
        merge_base,
        original_base,
        new_commits_base
    );

    // Always pass all commits through - let the authorship rewriting logic
    // handle many-to-one, one-to-one, and other mapping scenarios properly
    Ok((original_commits, new_commits))
}

fn walk_first_parent_commits(
    repository: &Repository,
    head: &str,
    base: &str,
    max_count: usize,
) -> Result<Vec<String>, crate::error::GitAiError> {
    if head == base || max_count == 0 {
        return Ok(Vec::new());
    }

    let mut args = repository.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--first-parent".to_string());
    args.push("--topo-order".to_string());
    args.push(format!("--max-count={}", max_count));
    args.push(format!("{}..{}", base, head));

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;
    let commits = stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    Ok(commits)
}

fn is_ancestor(repository: &Repository, ancestor: &str, descendant: &str) -> bool {
    let mut args = repository.global_args_for_exec();
    args.push("merge-base".to_string());
    args.push("--is-ancestor".to_string());
    args.push(ancestor.to_string());
    args.push(descendant.to_string());
    crate::git::repository::exec_git(&args).is_ok()
}
