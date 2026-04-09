use crate::authorship::rebase_authorship::walk_commits_to_base;
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::git::cli_parser::{ParsedGitInvocation, RebaseArgsSummary, is_dry_run};
use crate::git::repository::Repository;
use crate::git::rewrite_log::RewriteLogEvent;

pub fn pre_rebase_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    tracing::debug!("=== REBASE PRE-COMMAND HOOK ===");

    // Check if we're continuing an existing rebase or starting a new one
    let rebase_dir = repository.path().join("rebase-merge");
    let rebase_apply_dir = repository.path().join("rebase-apply");
    let rebase_in_progress = rebase_dir.exists() || rebase_apply_dir.exists();

    tracing::debug!(
        "Rebase directories check: rebase-merge={}, rebase-apply={}",
        rebase_dir.exists(),
        rebase_apply_dir.exists()
    );

    // Check if there's an active Start event in the log that matches
    let has_active_start = has_active_rebase_start_event(repository);
    let is_continuing = rebase_in_progress && has_active_start;

    tracing::debug!(
        "Rebase state: in_progress={}, has_active_start={}, is_continuing={}",
        rebase_in_progress,
        has_active_start,
        is_continuing
    );

    if !is_continuing {
        // Starting a new rebase - capture original HEAD and log Start event
        if let Ok(head) = repository.head() {
            if let Ok(target) = head.target() {
                let original_head = resolve_rebase_original_head(parsed_args, repository)
                    .unwrap_or_else(|| target.clone());
                let onto_head = resolve_rebase_onto_head(parsed_args, repository);
                tracing::debug!(
                    "Starting new rebase from HEAD: {} (resolved original_head: {}, onto: {:?})",
                    target,
                    original_head,
                    onto_head
                );
                command_hooks_context.rebase_original_head = Some(original_head.clone());
                command_hooks_context.rebase_onto = onto_head.clone();

                // Determine if interactive
                let is_interactive = parsed_args.has_command_flag("-i")
                    || parsed_args.has_command_flag("--interactive");

                tracing::debug!("Interactive rebase: {}", is_interactive);

                // Log the rebase start event
                let start_event = RewriteLogEvent::rebase_start(
                    crate::git::rewrite_log::RebaseStartEvent::new_with_onto(
                        original_head,
                        is_interactive,
                        onto_head,
                    ),
                );

                // Write to rewrite log
                match repository.storage.append_rewrite_event(start_event) {
                    Ok(_) => tracing::debug!("✓ Logged RebaseStart event"),
                    Err(e) => tracing::debug!("✗ Failed to log RebaseStart event: {}", e),
                }
            }
        } else {
            tracing::debug!("Could not read HEAD for new rebase");
        }
    } else {
        tracing::debug!(
            "Continuing existing rebase (will read original head from log in post-hook)"
        );
    }
}

pub fn handle_rebase_post_command(
    context: &CommandHooksContext,
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    tracing::debug!("=== REBASE POST-COMMAND HOOK ===");
    tracing::debug!("Exit status: {}", exit_status);

    // Check if rebase is still in progress
    let rebase_dir = repository.path().join("rebase-merge");
    let rebase_apply_dir = repository.path().join("rebase-apply");
    let is_in_progress = rebase_dir.exists() || rebase_apply_dir.exists();

    tracing::debug!(
        "Rebase directories check: rebase-merge={}, rebase-apply={}",
        rebase_dir.exists(),
        rebase_apply_dir.exists()
    );

    if is_in_progress {
        // Rebase still in progress (conflict or not finished)
        tracing::debug!(
            "⏸ Rebase still in progress, waiting for completion (conflict or multi-step)"
        );
        return;
    }

    if is_dry_run(&parsed_args.command_args) {
        tracing::debug!("Skipping rebase post-hook for dry-run");
        return;
    }

    // Rebase is done (completed or aborted)
    // Try to find original head / onto from context OR from the rewrite log
    let start_event_from_log = find_rebase_start_event(repository);
    let original_head_from_context = context.rebase_original_head.clone();
    let original_head_from_log = start_event_from_log
        .as_ref()
        .map(|event| event.original_head.clone());
    let onto_head_from_context = context.rebase_onto.clone();
    let onto_head_from_log = start_event_from_log
        .as_ref()
        .and_then(|event| event.onto_head.clone());

    tracing::debug!(
        "Original head: context={:?}, log={:?}; onto: context={:?}, log={:?}",
        original_head_from_context,
        original_head_from_log,
        onto_head_from_context,
        onto_head_from_log
    );

    let original_head = original_head_from_context.or(original_head_from_log);
    let onto_head = onto_head_from_context.or(onto_head_from_log);

    if !exit_status.success() {
        // Rebase was aborted or failed - log Abort event
        if let Some(orig_head) = original_head {
            tracing::debug!("✗ Rebase aborted/failed from {}", orig_head);
            let abort_event = RewriteLogEvent::rebase_abort(
                crate::git::rewrite_log::RebaseAbortEvent::new(orig_head),
            );
            match repository.storage.append_rewrite_event(abort_event) {
                Ok(_) => tracing::debug!("✓ Logged RebaseAbort event"),
                Err(e) => tracing::debug!("✗ Failed to log RebaseAbort event: {}", e),
            }
        } else {
            tracing::debug!("✗ Rebase failed but couldn't determine original head");
        }
        return;
    }

    // Rebase completed successfully!
    tracing::debug!("✓ Rebase completed successfully");
    if let Some(original_head) = original_head {
        tracing::debug!("Processing completed rebase from {}", original_head);
        process_completed_rebase(
            repository,
            &original_head,
            onto_head.as_deref(),
            parsed_args,
        );
    } else {
        tracing::debug!("⚠ Rebase completed but couldn't determine original head");
    }
}

/// Check if there's an active rebase Start event (not followed by Complete or Abort)
fn has_active_rebase_start_event(repository: &Repository) -> bool {
    let events = match repository.storage.read_rewrite_events() {
        Ok(events) => events,
        Err(_) => return false,
    };
    has_active_rebase_start_event_from(&events)
}

/// Check pre-read events for an active rebase Start (not followed by Complete or Abort).
fn has_active_rebase_start_event_from(events: &[RewriteLogEvent]) -> bool {
    // Events are newest-first
    // If we find Complete or Abort before Start, there's no active rebase
    // If we find Start before Complete/Abort, there's an active rebase
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { .. } | RewriteLogEvent::RebaseAbort { .. } => {
                return false; // Found completion/abort first, no active rebase
            }
            RewriteLogEvent::RebaseStart { .. } => {
                return true; // Found start first, active rebase
            }
            _ => continue,
        }
    }

    false // No rebase events found
}

/// Find the most recent Rebase Start event in the log.
fn find_rebase_start_event(
    repository: &Repository,
) -> Option<crate::git::rewrite_log::RebaseStartEvent> {
    let events = repository.storage.read_rewrite_events().ok()?;
    find_rebase_start_event_from(&events)
}

/// Find the most recent Rebase Start event from pre-read events.
fn find_rebase_start_event_from(
    events: &[RewriteLogEvent],
) -> Option<crate::git::rewrite_log::RebaseStartEvent> {
    // Find the most recent Start event (events are newest-first)
    // If we encounter a Complete or Abort before a Start, there's no active rebase
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { .. } | RewriteLogEvent::RebaseAbort { .. } => {
                return None;
            }
            RewriteLogEvent::RebaseStart { rebase_start } => {
                return Some(rebase_start.clone());
            }
            _ => continue,
        }
    }

    None
}

fn process_completed_rebase(
    repository: &mut Repository,
    original_head: &str,
    onto_head: Option<&str>,
    parsed_args: &ParsedGitInvocation,
) {
    tracing::debug!("--- Processing completed rebase from {} ---", original_head);

    // Get the new HEAD
    let new_head = match repository.head() {
        Ok(head) => match head.target() {
            Ok(target) => {
                tracing::debug!("New HEAD: {}", target);
                target
            }
            Err(e) => {
                tracing::debug!("✗ Failed to get HEAD target: {}", e);
                return;
            }
        },
        Err(e) => {
            tracing::debug!("✗ Failed to get HEAD: {}", e);
            return;
        }
    };

    // If HEAD didn't change, nothing to do
    if original_head == new_head {
        tracing::debug!("Rebase resulted in no changes (fast-forward or empty)");
        return;
    }

    // Build commit mappings
    tracing::debug!(
        "Building commit mappings: {} -> {}",
        original_head,
        new_head
    );
    let (original_commits, new_commits) =
        match build_rebase_commit_mappings(repository, original_head, &new_head, onto_head) {
            Ok(mappings) => {
                tracing::debug!(
                    "✓ Built mappings: {} original commits -> {} new commits",
                    mappings.0.len(),
                    mappings.1.len()
                );
                mappings
            }
            Err(e) => {
                tracing::debug!("✗ Failed to build rebase mappings: {}", e);
                return;
            }
        };

    if original_commits.is_empty() {
        tracing::debug!("No commits to rewrite authorship for");
        return;
    }
    if new_commits.is_empty() {
        tracing::debug!(
            "No new rebased commits detected (all commits were skipped/already upstream)"
        );
        return;
    }

    tracing::debug!("Original commits: {:?}", original_commits);
    tracing::debug!("New commits: {:?}", new_commits);

    // Determine rebase type
    let is_interactive =
        parsed_args.has_command_flag("-i") || parsed_args.has_command_flag("--interactive");
    tracing::debug!(
        "Rebase type: {}",
        if is_interactive {
            "interactive"
        } else {
            "normal"
        }
    );

    let rebase_event =
        RewriteLogEvent::rebase_complete(crate::git::rewrite_log::RebaseCompleteEvent::new(
            original_head.to_string(),
            new_head.clone(),
            is_interactive,
            original_commits.clone(),
            new_commits.clone(),
        ));

    tracing::debug!("Creating RebaseComplete event and rewriting authorship...");
    let commit_author = get_commit_default_author(repository, &parsed_args.command_args);

    repository.handle_rewrite_log_event(
        rebase_event,
        commit_author,
        false, // don't suppress output
        true,  // save to log
    );

    tracing::debug!("✓ Rebase authorship rewrite complete");
}

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

/// Walk first-parent commits from `head` back to `base`, returning at most
/// `max_count` commits.  Returns newest-first (same as `walk_commits_to_base`).
///
/// Rebased commits always form a linear first-parent chain at the tip of the
/// branch.  By following only first parents and capping at the number of source
/// commits we avoid sweeping in unrelated target-branch history (including merge
/// commits) when the walk base is too far back.
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

fn resolve_rebase_original_head(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<String> {
    let summary = summarize_rebase_args(parsed_args);
    if summary.is_control_mode {
        return None;
    }

    // Branch selection rules:
    // - `git rebase <upstream> <branch>` -> second positional
    // - `git rebase --root <branch>` -> first positional
    let branch_idx = if summary.has_root { 0 } else { 1 };
    let branch_spec = summary.positionals.get(branch_idx)?;
    resolve_commitish(repository, branch_spec)
}

pub(crate) fn resolve_rebase_onto_head(
    parsed_args: &ParsedGitInvocation,
    repository: &Repository,
) -> Option<String> {
    let summary = summarize_rebase_args(parsed_args);
    if summary.is_control_mode {
        return None;
    }

    if let Some(onto_spec) = summary.onto_spec {
        return resolve_commitish(repository, &onto_spec);
    }

    // `--root` mode has no implicit upstream bound unless `--onto` is provided.
    if summary.has_root {
        return None;
    }

    if let Some(upstream_spec) = summary.positionals.first() {
        return resolve_commitish(repository, upstream_spec);
    }

    // `git rebase` with no explicit upstream uses the current branch upstream.
    resolve_commitish(repository, "@{upstream}")
}

fn resolve_commitish(repository: &Repository, spec: &str) -> Option<String> {
    repository
        .revparse_single(spec)
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

fn summarize_rebase_args(parsed_args: &ParsedGitInvocation) -> RebaseArgsSummary {
    crate::git::cli_parser::summarize_rebase_args(&parsed_args.command_args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::cli_parser::ParsedGitInvocation;

    /// Build a `ParsedGitInvocation` whose `command` is "rebase" and whose
    /// `command_args` are the supplied strings.
    fn make_rebase_invocation(args: &[&str]) -> ParsedGitInvocation {
        ParsedGitInvocation {
            global_args: Vec::new(),
            command: Some("rebase".to_string()),
            command_args: args.iter().map(|s| s.to_string()).collect(),
            saw_end_of_opts: false,
            is_help: false,
        }
    }

    #[test]
    fn test_summarize_rebase_args_continue_is_control_mode() {
        let parsed = make_rebase_invocation(&["--continue"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(summary.is_control_mode);
    }

    #[test]
    fn test_summarize_rebase_args_abort_is_control_mode() {
        let parsed = make_rebase_invocation(&["--abort"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(summary.is_control_mode);
    }

    #[test]
    fn test_summarize_rebase_args_skip_is_control_mode() {
        let parsed = make_rebase_invocation(&["--skip"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(summary.is_control_mode);
    }

    #[test]
    fn test_summarize_rebase_args_upstream_only() {
        let parsed = make_rebase_invocation(&["origin/main"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(!summary.is_control_mode);
        assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
    }

    #[test]
    fn test_summarize_rebase_args_upstream_and_branch() {
        let parsed = make_rebase_invocation(&["origin/main", "feature"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(!summary.is_control_mode);
        assert_eq!(
            summary.positionals,
            vec!["origin/main".to_string(), "feature".to_string()]
        );
    }

    #[test]
    fn test_summarize_rebase_args_onto_flag() {
        let parsed = make_rebase_invocation(&["--onto", "abc123", "origin/main"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(!summary.is_control_mode);
        assert_eq!(summary.onto_spec, Some("abc123".to_string()));
        assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
    }

    #[test]
    fn test_summarize_rebase_args_onto_equals_flag() {
        let parsed = make_rebase_invocation(&["--onto=abc123", "origin/main"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(!summary.is_control_mode);
        assert_eq!(summary.onto_spec, Some("abc123".to_string()));
    }

    #[test]
    fn test_summarize_rebase_args_root_flag() {
        let parsed = make_rebase_invocation(&["--root"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(!summary.is_control_mode);
        assert!(summary.has_root);
    }

    #[test]
    fn test_summarize_rebase_args_interactive_with_upstream() {
        let parsed = make_rebase_invocation(&["-i", "origin/main"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(!summary.is_control_mode);
        assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
    }

    #[test]
    fn test_summarize_rebase_args_strategy_consumes_value() {
        let parsed = make_rebase_invocation(&["-s", "ours", "origin/main"]);
        let summary = summarize_rebase_args(&parsed);
        assert!(!summary.is_control_mode);
        assert_eq!(summary.positionals, vec!["origin/main".to_string()]);
    }

    #[test]
    fn test_build_rebase_commit_mappings_excludes_merge_commits_from_new_commits() {
        use crate::git::test_utils::TmpRepo;

        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit").expect("base");
        let base_sha = repo.get_head_commit_sha().expect("base sha");
        let default_branch = repo.current_branch().expect("branch");

        // Create a side branch with a commit
        repo.create_branch("side").expect("create side");
        repo.write_file("side.txt", "side\n", true)
            .expect("write side");
        repo.commit_with_message("side commit").expect("side");

        // Switch back to default branch, add a commit, then merge --no-ff
        repo.switch_branch(&default_branch).expect("switch");
        repo.write_file("main.txt", "main\n", true)
            .expect("write main");
        repo.commit_with_message("main commit").expect("main");

        repo.git_command(&["merge", "--no-ff", "side", "-m", "Merge side"])
            .expect("merge");
        let merge_sha = repo.get_head_commit_sha().expect("merge sha");

        // Create a feature branch from base, add a commit
        repo.git_command(&["checkout", "-b", "feature", &base_sha])
            .expect("feature branch");
        repo.write_file("feat.txt", "feat\n", true)
            .expect("write feat");
        repo.commit_with_message("feature commit").expect("feat");
        let original_head = repo.get_head_commit_sha().expect("original head");

        // Rebase feature onto the default branch (which has the merge commit)
        repo.git_command(&["rebase", &default_branch])
            .expect("rebase");
        let new_head = repo.get_head_commit_sha().expect("new head");

        // Call build_rebase_commit_mappings with onto_head = None
        // to simulate the fallback path (daemon / plumbing rewrite)
        let (original_commits, new_commits) =
            build_rebase_commit_mappings(repo.gitai_repo(), &original_head, &new_head, None)
                .expect("build mappings");

        // The merge commit should NOT be in new_commits
        assert!(
            !new_commits.contains(&merge_sha),
            "new_commits should not contain the merge commit {}, but got: {:?}",
            merge_sha,
            new_commits
        );

        // There should be exactly 1 original commit and 1 new commit
        assert_eq!(
            original_commits.len(),
            1,
            "Should have exactly 1 original commit, got: {:?}",
            original_commits
        );
        assert_eq!(
            new_commits.len(),
            1,
            "Should have exactly 1 new commit (the rebased feature), got: {:?}",
            new_commits
        );
    }

    #[test]
    fn test_build_rebase_commit_mappings_excludes_merge_commits_when_onto_equals_merge_base() {
        use crate::git::test_utils::TmpRepo;

        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit").expect("base");
        let base_sha = repo.get_head_commit_sha().expect("base sha");
        let default_branch = repo.current_branch().expect("branch");

        repo.create_branch("side").expect("create side");
        repo.write_file("side.txt", "side\n", true)
            .expect("write side");
        repo.commit_with_message("side commit").expect("side");

        repo.switch_branch(&default_branch).expect("switch");
        repo.write_file("main.txt", "main\n", true)
            .expect("write main");
        repo.commit_with_message("main commit").expect("main");

        repo.git_command(&["merge", "--no-ff", "side", "-m", "Merge side"])
            .expect("merge");
        let merge_sha = repo.get_head_commit_sha().expect("merge sha");

        repo.git_command(&["checkout", "-b", "feature", &base_sha])
            .expect("feature branch");
        repo.write_file("feat.txt", "feat\n", true)
            .expect("write feat");
        repo.commit_with_message("feature commit").expect("feat");
        let original_head = repo.get_head_commit_sha().expect("original head");

        repo.git_command(&["rebase", &default_branch])
            .expect("rebase");
        let new_head = repo.get_head_commit_sha().expect("new head");

        let computed_merge_base = repo
            .gitai_repo()
            .merge_base(original_head.clone(), new_head.clone())
            .expect("merge_base");

        let (original_commits, new_commits) = build_rebase_commit_mappings(
            repo.gitai_repo(),
            &original_head,
            &new_head,
            Some(&computed_merge_base),
        )
        .expect("build mappings");

        assert!(
            !new_commits.contains(&merge_sha),
            "new_commits should not contain merge commit {} when onto_head == merge_base, got: {:?}",
            merge_sha,
            new_commits
        );
        assert_eq!(original_commits.len(), 1);
        assert_eq!(new_commits.len(), 1);
    }

    #[test]
    fn test_build_rebase_commit_mappings_multi_commit_with_onto_equals_merge_base() {
        use crate::git::test_utils::TmpRepo;

        let repo = TmpRepo::new().expect("tmp repo");
        repo.write_file("base.txt", "base\n", true)
            .expect("write base");
        repo.commit_with_message("base commit").expect("base");
        let base_sha = repo.get_head_commit_sha().expect("base sha");
        let default_branch = repo.current_branch().expect("branch");

        repo.create_branch("side").expect("create side");
        repo.write_file("side.txt", "side\n", true)
            .expect("write side");
        repo.commit_with_message("side commit").expect("side");

        repo.switch_branch(&default_branch).expect("switch");
        repo.write_file("main.txt", "main\n", true)
            .expect("write main");
        repo.commit_with_message("main commit").expect("main");

        repo.git_command(&["merge", "--no-ff", "side", "-m", "Merge side"])
            .expect("merge");

        repo.git_command(&["checkout", "-b", "feature", &base_sha])
            .expect("feature branch");
        repo.write_file("feat1.txt", "feat1\n", true)
            .expect("write feat1");
        repo.commit_with_message("feature commit 1").expect("feat1");
        repo.write_file("feat2.txt", "feat2\n", true)
            .expect("write feat2");
        repo.commit_with_message("feature commit 2").expect("feat2");
        let original_head = repo.get_head_commit_sha().expect("original head");

        repo.git_command(&["rebase", &default_branch])
            .expect("rebase");
        let new_head = repo.get_head_commit_sha().expect("new head");

        let computed_merge_base = repo
            .gitai_repo()
            .merge_base(original_head.clone(), new_head.clone())
            .expect("merge_base");

        let (original_commits, new_commits) = build_rebase_commit_mappings(
            repo.gitai_repo(),
            &original_head,
            &new_head,
            Some(&computed_merge_base),
        )
        .expect("build mappings");

        assert_eq!(original_commits.len(), 2);
        assert_eq!(new_commits.len(), 2);
    }
}
