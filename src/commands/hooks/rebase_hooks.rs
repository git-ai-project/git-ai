use crate::authorship::rebase_authorship::walk_commits_to_base;
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::cli_parser::is_dry_run;
use crate::git::repository::Repository;
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::debug_log;

pub fn pre_rebase_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    debug_log("=== REBASE PRE-COMMAND HOOK ===");

    // Check if we're continuing an existing rebase or starting a new one
    let rebase_dir = repository.path().join("rebase-merge");
    let rebase_apply_dir = repository.path().join("rebase-apply");
    let rebase_in_progress = rebase_dir.exists() || rebase_apply_dir.exists();

    debug_log(&format!(
        "Rebase directories check: rebase-merge={}, rebase-apply={}",
        rebase_dir.exists(),
        rebase_apply_dir.exists()
    ));

    // Check if there's an active Start event in the log that matches
    let has_active_start = has_active_rebase_start_event(repository);
    let is_continuing = rebase_in_progress && has_active_start;

    debug_log(&format!(
        "Rebase state: in_progress={}, has_active_start={}, is_continuing={}",
        rebase_in_progress, has_active_start, is_continuing
    ));

    if !is_continuing {
        // Starting a new rebase - capture original HEAD and log Start event
        if let Ok(head) = repository.head() {
            if let Ok(target) = head.target() {
                let original_head = resolve_rebase_original_head(parsed_args, repository)
                    .unwrap_or_else(|| target.clone());
                let onto_head = resolve_rebase_onto_head(parsed_args, repository);
                debug_log(&format!(
                    "Starting new rebase from HEAD: {} (resolved original_head: {}, onto: {:?})",
                    target, original_head, onto_head
                ));
                command_hooks_context.rebase_original_head = Some(original_head.clone());
                command_hooks_context.rebase_onto = onto_head.clone();

                // Determine if interactive
                let is_interactive = parsed_args.has_command_flag("-i")
                    || parsed_args.has_command_flag("--interactive");

                debug_log(&format!("Interactive rebase: {}", is_interactive));

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
                    Ok(_) => debug_log("✓ Logged RebaseStart event"),
                    Err(e) => debug_log(&format!("✗ Failed to log RebaseStart event: {}", e)),
                }
            }
        } else {
            debug_log("Could not read HEAD for new rebase");
        }
    } else {
        debug_log("Continuing existing rebase (will read original head from log in post-hook)");
    }
}

pub fn handle_rebase_post_command(
    context: &CommandHooksContext,
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    debug_log("=== REBASE POST-COMMAND HOOK ===");
    debug_log(&format!("Exit status: {}", exit_status));

    // Check if rebase is still in progress
    let rebase_dir = repository.path().join("rebase-merge");
    let rebase_apply_dir = repository.path().join("rebase-apply");
    let is_in_progress = rebase_dir.exists() || rebase_apply_dir.exists();

    debug_log(&format!(
        "Rebase directories check: rebase-merge={}, rebase-apply={}",
        rebase_dir.exists(),
        rebase_apply_dir.exists()
    ));

    if is_in_progress {
        // Rebase still in progress (conflict or not finished)
        debug_log("⏸ Rebase still in progress, waiting for completion (conflict or multi-step)");
        return;
    }

    if is_dry_run(&parsed_args.command_args) {
        debug_log("Skipping rebase post-hook for dry-run");
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

    debug_log(&format!(
        "Original head: context={:?}, log={:?}; onto: context={:?}, log={:?}",
        original_head_from_context,
        original_head_from_log,
        onto_head_from_context,
        onto_head_from_log
    ));

    let original_head = original_head_from_context.or(original_head_from_log);
    let onto_head = onto_head_from_context.or(onto_head_from_log);

    if !exit_status.success() {
        // Rebase was aborted or failed - log Abort event
        if let Some(orig_head) = original_head {
            debug_log(&format!("✗ Rebase aborted/failed from {}", orig_head));
            let abort_event = RewriteLogEvent::rebase_abort(
                crate::git::rewrite_log::RebaseAbortEvent::new(orig_head),
            );
            match repository.storage.append_rewrite_event(abort_event) {
                Ok(_) => debug_log("✓ Logged RebaseAbort event"),
                Err(e) => debug_log(&format!("✗ Failed to log RebaseAbort event: {}", e)),
            }
        } else {
            debug_log("✗ Rebase failed but couldn't determine original head");
        }
        return;
    }

    // Rebase completed successfully!
    debug_log("✓ Rebase completed successfully");
    if let Some(original_head) = original_head {
        debug_log(&format!(
            "Processing completed rebase from {}",
            original_head
        ));
        process_completed_rebase(
            repository,
            &original_head,
            onto_head.as_deref(),
            parsed_args,
        );
    } else {
        debug_log("⚠ Rebase completed but couldn't determine original head");
    }
}

/// Check if there's an active rebase Start event (not followed by Complete or Abort)
fn has_active_rebase_start_event(repository: &Repository) -> bool {
    let events = match repository.storage.read_rewrite_events() {
        Ok(events) => events,
        Err(_) => return false,
    };

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

    // Find the most recent Start event (events are newest-first)
    for event in events {
        match event {
            RewriteLogEvent::RebaseStart { rebase_start } => {
                return Some(rebase_start);
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
    debug_log(&format!(
        "--- Processing completed rebase from {} ---",
        original_head
    ));

    // Get the new HEAD
    let new_head = match repository.head() {
        Ok(head) => match head.target() {
            Ok(target) => {
                debug_log(&format!("New HEAD: {}", target));
                target
            }
            Err(e) => {
                debug_log(&format!("✗ Failed to get HEAD target: {}", e));
                return;
            }
        },
        Err(e) => {
            debug_log(&format!("✗ Failed to get HEAD: {}", e));
            return;
        }
    };

    // If HEAD didn't change, nothing to do
    if original_head == new_head {
        debug_log("Rebase resulted in no changes (fast-forward or empty)");
        return;
    }

    // Build commit mappings
    debug_log(&format!(
        "Building commit mappings: {} -> {}",
        original_head, new_head
    ));
    let (original_commits, new_commits) =
        match build_rebase_commit_mappings(repository, original_head, &new_head, onto_head) {
            Ok(mappings) => {
                debug_log(&format!(
                    "✓ Built mappings: {} original commits -> {} new commits",
                    mappings.0.len(),
                    mappings.1.len()
                ));
                mappings
            }
            Err(e) => {
                debug_log(&format!("✗ Failed to build rebase mappings: {}", e));
                return;
            }
        };

    if original_commits.is_empty() {
        debug_log("No commits to rewrite authorship for");
        return;
    }
    if new_commits.is_empty() {
        debug_log("No new rebased commits detected (all commits were skipped/already upstream)");
        return;
    }

    debug_log(&format!("Original commits: {:?}", original_commits));
    debug_log(&format!("New commits: {:?}", new_commits));

    // Determine rebase type
    let is_interactive =
        parsed_args.has_command_flag("-i") || parsed_args.has_command_flag("--interactive");
    debug_log(&format!(
        "Rebase type: {}",
        if is_interactive {
            "interactive"
        } else {
            "normal"
        }
    ));

    let rebase_event =
        RewriteLogEvent::rebase_complete(crate::git::rewrite_log::RebaseCompleteEvent::new(
            original_head.to_string(),
            new_head.clone(),
            is_interactive,
            original_commits.clone(),
            new_commits.clone(),
        ));

    debug_log("Creating RebaseComplete event and rewriting authorship...");
    let commit_author = get_commit_default_author(repository, &parsed_args.command_args);

    repository.handle_rewrite_log_event(
        rebase_event,
        commit_author,
        false, // don't suppress output
        true,  // save to log
    );

    debug_log("✓ Rebase authorship rewrite complete");
}

pub(crate) fn build_rebase_commit_mappings(
    repository: &Repository,
    original_head: &str,
    new_head: &str,
    onto_head: Option<&str>,
) -> Result<(Vec<String>, Vec<String>), crate::error::GitAiError> {
    // Get commits from new_head and original_head
    let new_head_commit = repository.find_commit(new_head.to_string())?;
    let original_head_commit = repository.find_commit(original_head.to_string())?;

    // Find merge base between original and new
    let merge_base = repository.merge_base(original_head_commit.id(), new_head_commit.id())?;

    // Walk from original_head to merge_base to get the commits that were rebased
    let mut original_commits = walk_commits_to_base(repository, original_head, &merge_base)?;
    original_commits.reverse();

    // If there were no original commits, there is nothing to rewrite.
    // Avoid walking potentially large parts of new history.
    if original_commits.is_empty() {
        debug_log(&format!(
            "Commit mapping: 0 original -> 0 new (merge_base: {})",
            merge_base
        ));
        return Ok((original_commits, Vec::new()));
    }

    // Prefer the rebase target (onto) as the lower bound for new commits. This prevents
    // skipped/no-op rebases from sweeping unrelated target-branch history.
    let new_commits_base = onto_head
        .filter(|onto| is_ancestor(repository, onto, new_head))
        .unwrap_or(merge_base.as_str());

    // Walk from new_head to base to get the actual rebased commits
    let mut new_commits = walk_commits_to_base(repository, new_head, new_commits_base)?;

    // Reverse so they're in chronological order (oldest first)
    new_commits.reverse();

    debug_log(&format!(
        "Commit mapping: {} original -> {} new (merge_base: {}, new_base: {})",
        original_commits.len(),
        new_commits.len(),
        merge_base,
        new_commits_base
    ));

    // Always pass all commits through - let the authorship rewriting logic
    // handle many-to-one, one-to-one, and other mapping scenarios properly
    Ok((original_commits, new_commits))
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

fn resolve_rebase_onto_head(
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

struct RebaseArgsSummary {
    is_control_mode: bool,
    has_root: bool,
    onto_spec: Option<String>,
    positionals: Vec<String>,
}

fn summarize_rebase_args(parsed_args: &ParsedGitInvocation) -> RebaseArgsSummary {
    // Modes that do not start a new rebase sequence.
    for mode in [
        "--continue",
        "--abort",
        "--skip",
        "--quit",
        "--show-current-patch",
    ] {
        if parsed_args.has_command_flag(mode) {
            return RebaseArgsSummary {
                is_control_mode: true,
                has_root: false,
                onto_spec: None,
                positionals: Vec::new(),
            };
        }
    }

    let mut has_root = false;
    let mut onto_spec: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let args = &parsed_args.command_args;
    let mut i = 0usize;

    while i < args.len() {
        let arg = args[i].as_str();

        if arg == "--" {
            break;
        }

        if arg == "--onto" {
            if let Some(next) = args.get(i + 1) {
                onto_spec = Some(next.clone());
                i += 2;
                continue;
            }
            break;
        }
        if let Some(spec) = arg.strip_prefix("--onto=") {
            onto_spec = Some(spec.to_string());
            i += 1;
            continue;
        }

        if arg == "--root" {
            has_root = true;
            i += 1;
            continue;
        }

        if arg.starts_with('-') {
            // Subset of rebase flags that consume a separate value token.
            let takes_value = matches!(
                arg,
                "-s" | "--strategy"
                    | "-X"
                    | "--strategy-option"
                    | "-x"
                    | "--exec"
                    | "--empty"
                    | "-C"
                    | "-S"
                    | "--gpg-sign"
            );
            if takes_value && !arg.contains('=') {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        positionals.push(arg.to_string());
        i += 1;
    }

    RebaseArgsSummary {
        is_control_mode: false,
        has_root,
        onto_spec,
        positionals,
    }
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
}
