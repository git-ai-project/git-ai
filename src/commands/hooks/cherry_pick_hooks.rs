use crate::authorship::rebase_authorship::walk_commits_to_base;
use crate::commands::git_handlers::CommandHooksContext;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::repository::Repository;
use crate::git::rewrite_log::RewriteLogEvent;
use crate::utils::debug_log;

pub fn pre_cherry_pick_hook(
    parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    _command_hooks_context: &mut CommandHooksContext,
) {
    debug_log("=== CHERRY-PICK PRE-COMMAND HOOK ===");

    // Check if we're continuing an existing cherry-pick or starting a new one
    let cherry_pick_head = repository.path().join("CHERRY_PICK_HEAD");
    let sequencer_dir = repository.path().join("sequencer");
    let cherry_pick_in_progress = cherry_pick_head.exists() || sequencer_dir.exists();

    debug_log(&format!(
        "Cherry-pick state check: CHERRY_PICK_HEAD={}, sequencer={}",
        cherry_pick_head.exists(),
        sequencer_dir.exists()
    ));

    // Check if there's an active Start event in the log that matches
    let has_active_start = has_active_cherry_pick_start_event(repository);
    let is_continuing = cherry_pick_in_progress && has_active_start;

    debug_log(&format!(
        "Cherry-pick state: in_progress={}, has_active_start={}, is_continuing={}",
        cherry_pick_in_progress, has_active_start, is_continuing
    ));

    if !is_continuing {
        // Starting a new cherry-pick - capture original HEAD and log Start event
        if let Ok(head) = repository.head() {
            if let Ok(target) = head.target() {
                debug_log(&format!("Starting new cherry-pick from HEAD: {}", target));

                // Parse source commits from args
                let source_commits =
                    parse_cherry_pick_commits(repository, &parsed_args.command_args);

                debug_log(&format!(
                    "Cherry-picking {} commits: {:?}",
                    source_commits.len(),
                    source_commits
                ));

                // Fix #952: If source_commits is empty (e.g. bad args), skip writing
                // the Start event to prevent state corruption for subsequent operations.
                if source_commits.is_empty() {
                    debug_log(
                        "No valid source commits parsed, skipping CherryPickStart event (prevents state corruption from bad args)",
                    );
                    return;
                }

                // Log the cherry-pick start event
                let start_event = RewriteLogEvent::cherry_pick_start(
                    crate::git::rewrite_log::CherryPickStartEvent::new(
                        target.clone(),
                        source_commits,
                    ),
                );

                // Write to rewrite log
                match repository.storage.append_rewrite_event(start_event) {
                    Ok(_) => debug_log("✓ Logged CherryPickStart event"),
                    Err(e) => debug_log(&format!("✗ Failed to log CherryPickStart event: {}", e)),
                }
            }
        } else {
            debug_log("Could not read HEAD for new cherry-pick");
        }
    } else {
        debug_log(
            "Continuing existing cherry-pick (will read original head from log in post-hook)",
        );
        // Fix #951: If --skip is being used, update source_commits to remove
        // the skipped commit so subsequent cherry-picks get correct attribution.
        if parsed_args.command_args.iter().any(|a| a == "--skip") {
            handle_cherry_pick_skip(repository);
        }
    }
}

pub fn post_cherry_pick_hook(
    _context: &CommandHooksContext,
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    debug_log("=== CHERRY-PICK POST-COMMAND HOOK ===");
    debug_log(&format!("Exit status: {}", exit_status));

    // Check if cherry-pick is still in progress
    let cherry_pick_head = repository.path().join("CHERRY_PICK_HEAD");
    let sequencer_dir = repository.path().join("sequencer");
    let is_in_progress = cherry_pick_head.exists() || sequencer_dir.exists();

    debug_log(&format!(
        "Cherry-pick state check: CHERRY_PICK_HEAD={}, sequencer={}",
        cherry_pick_head.exists(),
        sequencer_dir.exists()
    ));

    if is_in_progress {
        // Cherry-pick still in progress (conflict or not finished)
        debug_log(
            "⏸ Cherry-pick still in progress, waiting for completion (conflict or multi-step)",
        );
        return;
    }

    if is_dry_run(&parsed_args.command_args) {
        debug_log("Skipping cherry-pick post-hook for dry-run");
        return;
    }

    // Cherry-pick is done (completed or aborted)
    // Try to find the original head from the rewrite log
    let original_head = find_cherry_pick_start_event_original_head(repository);

    debug_log(&format!("Original head from log: {:?}", original_head));

    if !exit_status.success() {
        // Cherry-pick was aborted or failed - log Abort event
        if let Some(orig_head) = original_head {
            debug_log(&format!("✗ Cherry-pick aborted/failed from {}", orig_head));
            let abort_event = RewriteLogEvent::cherry_pick_abort(
                crate::git::rewrite_log::CherryPickAbortEvent::new(orig_head),
            );
            match repository.storage.append_rewrite_event(abort_event) {
                Ok(_) => debug_log("✓ Logged CherryPickAbort event"),
                Err(e) => debug_log(&format!("✗ Failed to log CherryPickAbort event: {}", e)),
            }
        } else {
            debug_log("✗ Cherry-pick failed but couldn't determine original head");
        }
        return;
    }

    // Cherry-pick completed successfully!
    debug_log("✓ Cherry-pick completed successfully");
    if let Some(original_head) = original_head {
        debug_log(&format!(
            "Processing completed cherry-pick from {}",
            original_head
        ));
        process_completed_cherry_pick(repository, &original_head, parsed_args);
    } else {
        debug_log("⚠ Cherry-pick completed but couldn't determine original head");
    }
}

/// Check if there's an active cherry-pick Start event (not followed by Complete or Abort)
fn has_active_cherry_pick_start_event(repository: &Repository) -> bool {
    let events = match repository.storage.read_rewrite_events() {
        Ok(events) => events,
        Err(_) => return false,
    };

    // Events are newest-first
    // If we find Complete or Abort before Start, there's no active cherry-pick
    // If we find Start before Complete/Abort, there's an active cherry-pick
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                return false; // Found completion/abort first, no active cherry-pick
            }
            RewriteLogEvent::CherryPickStart { .. } => {
                return true; // Found start first, active cherry-pick
            }
            _ => continue,
        }
    }

    false // No cherry-pick events found
}

/// Find the original head from the most recent CherryPick Start event in the log.
/// Stops at Complete/Abort events so orphaned Start events from a prior aborted
/// cherry-pick are not mistakenly returned.
fn find_cherry_pick_start_event_original_head(repository: &Repository) -> Option<String> {
    let events = repository.storage.read_rewrite_events().ok()?;

    // Events are newest-first; stop at Complete/Abort before finding a Start.
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                return None; // No active cherry-pick
            }
            RewriteLogEvent::CherryPickStart { cherry_pick_start } => {
                return Some(cherry_pick_start.original_head.clone());
            }
            _ => continue,
        }
    }

    None
}

/// Find the source commits from the most recent CherryPick Start event in the log.
/// Stops at Complete/Abort events so orphaned Start events from a prior aborted
/// cherry-pick are not mistakenly returned.
fn find_cherry_pick_start_event_source_commits(repository: &Repository) -> Option<Vec<String>> {
    let events = repository.storage.read_rewrite_events().ok()?;

    // Events are newest-first; stop at Complete/Abort before finding a Start.
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                return None; // No active cherry-pick
            }
            RewriteLogEvent::CherryPickStart { cherry_pick_start } => {
                return Some(cherry_pick_start.source_commits.clone());
            }
            _ => continue,
        }
    }

    None
}

/// Parse cherry-pick commit arguments
/// Handles:
/// - Single commit: `git cherry-pick A`
/// - Multiple commits: `git cherry-pick A B C`
/// - Ranges: `git cherry-pick A..C` or `git cherry-pick A^..C`
fn parse_cherry_pick_commits(repository: &Repository, args: &[String]) -> Vec<String> {
    let mut commits = Vec::new();

    // Filter out flags and options
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        // Skip flags and their values
        if arg.starts_with('-') {
            // Skip option values for flags that take arguments
            if arg == "-m" || arg == "--mainline" || arg == "-s" || arg == "--strategy" {
                i += 2; // Skip flag and its value
                continue;
            }
            i += 1;
            continue;
        }

        // Skip special keywords
        if arg == "continue" || arg == "abort" || arg == "quit" || arg == "skip" {
            i += 1;
            continue;
        }

        // This is a commit reference
        let commit_ref = arg.clone();

        // Check if it's a range (contains ..)
        if commit_ref.contains("..") {
            // Expand the range
            if let Ok(expanded) = expand_commit_range(repository, &commit_ref) {
                commits.extend(expanded);
            }
        } else {
            // Single commit - resolve it
            if let Ok(resolved) = resolve_commit_sha(repository, &commit_ref) {
                commits.push(resolved);
            }
        }

        i += 1;
    }

    commits
}

/// Expand a commit range like A..B or A^..B into a list of commits
fn expand_commit_range(
    repository: &Repository,
    range: &str,
) -> Result<Vec<String>, crate::error::GitAiError> {
    // Use git rev-list to expand the range
    let mut args = repository.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--reverse".to_string()); // Oldest first
    args.push(range.to_string());

    let output = crate::git::repository::exec_git(&args)?;
    let commits = String::from_utf8(output.stdout)?
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    Ok(commits)
}

/// Resolve a commit reference to its full SHA
fn resolve_commit_sha(
    repository: &Repository,
    commit_ref: &str,
) -> Result<String, crate::error::GitAiError> {
    let mut args = repository.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push(commit_ref.to_string());

    let output = crate::git::repository::exec_git(&args)?;
    let sha = String::from_utf8(output.stdout)?.trim().to_string();

    Ok(sha)
}

/// Fix #951: Handle cherry-pick --skip by removing the skipped commit from source_commits.
/// This prevents the skipped commit from corrupting attribution for remaining commits.
fn handle_cherry_pick_skip(repository: &mut Repository) {
    // Read CHERRY_PICK_HEAD to find which commit is being skipped
    let cherry_pick_head = repository.path().join("CHERRY_PICK_HEAD");
    let skipped_sha = match std::fs::read_to_string(&cherry_pick_head) {
        Ok(content) => content.trim().to_string(),
        Err(_) => {
            debug_log("Could not read CHERRY_PICK_HEAD for skip handling");
            return;
        }
    };

    debug_log(&format!(
        "Skipping commit {}, updating CherryPickStart source_commits",
        skipped_sha
    ));

    // Find the most recent CherryPickStart event (events are newest-first).
    // Stop if we hit a Complete or Abort (no active cherry-pick).
    let events = match repository.storage.read_rewrite_events() {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut current_start: Option<crate::git::rewrite_log::CherryPickStartEvent> = None;
    for event in events {
        match event {
            RewriteLogEvent::CherryPickComplete { .. }
            | RewriteLogEvent::CherryPickAbort { .. } => {
                break; // No active start
            }
            RewriteLogEvent::CherryPickStart { cherry_pick_start } => {
                current_start = Some(cherry_pick_start);
                break;
            }
            _ => continue,
        }
    }

    let start = match current_start {
        Some(s) => s,
        None => {
            debug_log("No active CherryPickStart event found for skip handling");
            return;
        }
    };

    // Remove the skipped commit from source_commits
    let updated_source_commits: Vec<String> = start
        .source_commits
        .iter()
        .filter(|sha| sha.as_str() != skipped_sha.as_str())
        .cloned()
        .collect();

    if updated_source_commits.len() == start.source_commits.len() {
        debug_log(&format!(
            "Skipped commit {} not found in source_commits, no update needed",
            skipped_sha
        ));
        return;
    }

    debug_log(&format!(
        "Updated source_commits: {} -> {} (removed {})",
        start.source_commits.len(),
        updated_source_commits.len(),
        skipped_sha
    ));

    // Write a new CherryPickStart event with updated source_commits.
    // Since find_cherry_pick_start_event_source_commits returns the most recent
    // Start event (newest-first), the new event will be read first.
    let new_start_event =
        RewriteLogEvent::cherry_pick_start(crate::git::rewrite_log::CherryPickStartEvent::new(
            start.original_head.clone(),
            updated_source_commits,
        ));

    match repository.storage.append_rewrite_event(new_start_event) {
        Ok(_) => debug_log("Updated CherryPickStart event for --skip"),
        Err(e) => debug_log(&format!("Failed to update CherryPickStart event: {}", e)),
    }
}

fn process_completed_cherry_pick(
    repository: &mut Repository,
    original_head: &str,
    parsed_args: &ParsedGitInvocation,
) {
    debug_log(&format!(
        "--- Processing completed cherry-pick from {} ---",
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
        debug_log("Cherry-pick resulted in no changes");
        return;
    }

    // Get source commits from the Start event
    let source_commits = match find_cherry_pick_start_event_source_commits(repository) {
        Some(commits) => {
            debug_log(&format!("Source commits from log: {:?}", commits));
            commits
        }
        None => {
            debug_log("✗ Could not find source commits from CherryPickStart event");
            return;
        }
    };

    // Build commit mappings
    debug_log(&format!(
        "Building commit mappings: {} -> {}",
        original_head, new_head
    ));
    let new_commits = match build_cherry_pick_commit_mappings(repository, original_head, &new_head)
    {
        Ok(commits) => {
            debug_log(&format!(
                "✓ Built mappings: {} source commits -> {} new commits",
                source_commits.len(),
                commits.len()
            ));
            commits
        }
        Err(e) => {
            debug_log(&format!("✗ Failed to build cherry-pick mappings: {}", e));
            return;
        }
    };

    if new_commits.is_empty() {
        debug_log("No commits to rewrite authorship for");
        return;
    }

    debug_log(&format!("Source commits: {:?}", source_commits));
    debug_log(&format!("New commits: {:?}", new_commits));

    let cherry_pick_event = RewriteLogEvent::cherry_pick_complete(
        crate::git::rewrite_log::CherryPickCompleteEvent::new(
            original_head.to_string(),
            new_head.clone(),
            source_commits.clone(),
            new_commits.clone(),
        ),
    );

    debug_log("Creating CherryPickComplete event and rewriting authorship...");
    let commit_author = get_commit_default_author(repository, &parsed_args.command_args);

    repository.handle_rewrite_log_event(
        cherry_pick_event,
        commit_author,
        false, // don't suppress output
        true,  // save to log
    );

    debug_log("✓ Cherry-pick authorship rewrite complete");
}

fn build_cherry_pick_commit_mappings(
    repository: &Repository,
    original_head: &str,
    new_head: &str,
) -> Result<Vec<String>, crate::error::GitAiError> {
    // Walk from new_head back to original_head to get the newly created commits
    let new_commits = walk_commits_to_base(repository, new_head, original_head)?;

    // Reverse to get chronological order (oldest first)
    let mut new_commits = new_commits;
    new_commits.reverse();

    debug_log(&format!(
        "Cherry-pick created {} new commits",
        new_commits.len()
    ));

    Ok(new_commits)
}
