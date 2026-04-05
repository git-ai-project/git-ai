use crate::commands::git_handlers::CommandHooksContext;
use crate::git::cli_parser::{ParsedGitInvocation, is_dry_run};
use crate::git::refs::{get_reference_as_authorship_log_v3, notes_add};
use crate::git::repository::Repository;
use crate::git::rewrite_log::{RevertMixedEvent, RewriteLogEvent};
use crate::utils::debug_log;

/// Pre-revert hook: capture the commit being reverted and store it for the post-hook.
pub fn pre_revert_hook(
    _parsed_args: &ParsedGitInvocation,
    repository: &mut Repository,
    command_hooks_context: &mut CommandHooksContext,
) {
    debug_log("=== REVERT PRE-COMMAND HOOK ===");

    // Capture the current HEAD before the revert
    if let Ok(head) = repository.head() {
        if let Ok(target) = head.target() {
            debug_log(&format!("Pre-revert HEAD: {}", target));
            command_hooks_context.revert_original_head = Some(target);
        }
    }
}

/// Post-revert hook: copy attribution from the source commit chain to the new revert commit.
pub fn post_revert_hook(
    command_hooks_context: &mut CommandHooksContext,
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    debug_log("=== REVERT POST-COMMAND HOOK ===");
    debug_log(&format!("Exit status: {}", exit_status));

    if !exit_status.success() {
        debug_log("Revert failed or was aborted, skipping attribution");
        return;
    }

    if is_dry_run(&parsed_args.command_args) {
        debug_log("Skipping revert post-hook for dry-run");
        return;
    }

    let original_head = match &command_hooks_context.revert_original_head {
        Some(head) => head.clone(),
        None => {
            debug_log("No original head captured in pre-revert hook");
            return;
        }
    };

    // Get the new HEAD (the revert commit)
    let new_head = match repository.head() {
        Ok(head) => match head.target() {
            Ok(target) => target,
            Err(e) => {
                debug_log(&format!("Failed to get HEAD target: {}", e));
                return;
            }
        },
        Err(e) => {
            debug_log(&format!("Failed to get HEAD: {}", e));
            return;
        }
    };

    if original_head == new_head {
        debug_log("HEAD did not change, nothing to do");
        return;
    }

    debug_log(&format!(
        "Revert created new commit: {} (was: {})",
        new_head, original_head
    ));

    // The reverted commit is the parent of the new commit that is NOT the original head.
    // For `git revert <sha>`, the new commit's parent is original_head, and <sha> is what
    // was reverted. We need to find <sha>.
    // The reverted commit SHA can be found by looking at the new commit's parent and
    // working backwards, or by reading REVERT_HEAD (which is gone after successful revert).
    // The most reliable approach: the reverted commit is original_head's successor if this
    // was a simple revert. Actually, let's parse from the commit message or args.
    let reverted_commit = find_reverted_commit(repository, parsed_args, &original_head);

    let reverted_commit = match reverted_commit {
        Some(sha) => {
            debug_log(&format!("Reverted commit: {}", sha));
            sha
        }
        None => {
            debug_log("Could not determine which commit was reverted");
            return;
        }
    };

    // Try to find AI attribution to copy to the new commit.
    // Strategy:
    // 1. Check if the reverted commit has an AI note with ai_additions > 0
    // 2. If not (e.g., it was a revert commit itself with no AI), check its parent
    //    (one level of chain traversal for revert-of-revert)
    let source_sha = find_attribution_source(repository, &reverted_commit);

    let source_sha = match source_sha {
        Some(sha) => {
            debug_log(&format!("Attribution source commit: {}", sha));
            sha
        }
        None => {
            debug_log("No AI attribution found in revert chain, skipping");
            // Still emit the RevertMixed event for logging
            emit_revert_event(repository, &reverted_commit, &new_head);
            return;
        }
    };

    // Copy the attribution note from source to the new commit
    copy_attribution_note(repository, &source_sha, &new_head);

    // Emit the RevertMixed event
    emit_revert_event(repository, &reverted_commit, &new_head);

    debug_log("Revert attribution handling complete");
}

/// Find the commit that was reverted by parsing args or commit message.
fn find_reverted_commit(
    repository: &Repository,
    parsed_args: &ParsedGitInvocation,
    _original_head: &str,
) -> Option<String> {
    // First, try to find the reverted commit from the command args
    // `git revert <commit>` — the commit ref is in the args
    for arg in &parsed_args.command_args {
        // Skip flags and options
        if arg.starts_with('-') {
            continue;
        }
        // Skip special keywords
        if arg == "continue" || arg == "abort" || arg == "quit" || arg == "skip" {
            continue;
        }

        // Try to resolve this as a commit SHA
        let mut args = repository.global_args_for_exec();
        args.push("rev-parse".to_string());
        args.push(arg.clone());

        if let Ok(output) = crate::git::repository::exec_git(&args) {
            let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
            if !sha.is_empty() {
                return Some(sha);
            }
        }
    }

    None
}

/// Find the commit whose attribution should be used for a revert.
///
/// Key insight: reverting an AI commit (which adds AI lines) creates a deletion commit —
/// no AI attribution should be copied. But reverting a revert (which restores AI lines)
/// should copy the original AI attribution.
///
/// Strategy:
/// - If the reverted commit HAS AI attribution, this revert is undoing AI work (deletion).
///   Do NOT copy attribution — the revert removes those lines.
/// - If the reverted commit has NO AI attribution, it might be a revert commit itself.
///   Check its parent (one level of chain traversal) for AI attribution to restore.
fn find_attribution_source(repository: &Repository, reverted_commit: &str) -> Option<String> {
    if has_ai_attribution(repository, reverted_commit) {
        // The reverted commit has AI attribution. This means we're undoing AI work
        // (the revert deletes AI-authored lines). Don't copy any attribution.
        debug_log(&format!(
            "Reverted commit {} has AI attribution — this revert undoes AI work, no attribution to copy",
            reverted_commit
        ));
        return None;
    }

    // The reverted commit has no AI attribution. This might be because:
    // - It was a revert commit itself (pure deletion → ai_additions=0)
    // - It was a human-only commit
    //
    // For revert-of-revert: check the reverted commit's first parent.
    // History: ... → A(ai) → B(revert of A, no ai) → C(revert of B, restores A's content)
    // B's parent is A. If A has AI attribution, copy it to C.

    debug_log(&format!(
        "Reverted commit {} has no AI attribution, checking parent (revert-of-revert case)",
        reverted_commit
    ));

    let parent_sha = get_first_parent(repository, reverted_commit)?;

    debug_log(&format!("Parent of reverted commit: {}", parent_sha));

    if has_ai_attribution(repository, &parent_sha) {
        return Some(parent_sha);
    }

    None
}

/// Check if a commit has an authorship note with ai_additions > 0.
fn has_ai_attribution(repository: &Repository, commit_sha: &str) -> bool {
    match get_reference_as_authorship_log_v3(repository, commit_sha) {
        Ok(log) => {
            // Check if any prompt record has accepted_lines > 0
            let has_ai = log.metadata.prompts.values().any(|p| p.accepted_lines > 0);
            debug_log(&format!(
                "Commit {} has_ai_attribution: {} (prompts: {})",
                commit_sha,
                has_ai,
                log.metadata.prompts.len()
            ));
            has_ai
        }
        Err(_) => {
            debug_log(&format!(
                "Commit {} has no parseable authorship note",
                commit_sha
            ));
            false
        }
    }
}

/// Get the first parent of a commit.
fn get_first_parent(repository: &Repository, commit_sha: &str) -> Option<String> {
    match repository.find_commit(commit_sha.to_string()) {
        Ok(commit) => {
            let mut parents = commit.parents();
            parents.next().map(|p| p.id().to_string())
        }
        Err(e) => {
            debug_log(&format!("Failed to find commit {}: {}", commit_sha, e));
            None
        }
    }
}

/// Copy an attribution note from one commit to another, updating the base_commit_sha.
fn copy_attribution_note(repository: &Repository, source_sha: &str, target_sha: &str) {
    match get_reference_as_authorship_log_v3(repository, source_sha) {
        Ok(mut log) => {
            // Update the base_commit_sha to point to the new commit
            log.metadata.base_commit_sha = target_sha.to_string();

            match log.serialize_to_string() {
                Ok(serialized) => match notes_add(repository, target_sha, &serialized) {
                    Ok(_) => {
                        debug_log(&format!(
                            "Copied attribution from {} to {}",
                            source_sha, target_sha
                        ));
                    }
                    Err(e) => {
                        debug_log(&format!("Failed to add note to {}: {}", target_sha, e));
                    }
                },
                Err(e) => {
                    debug_log(&format!("Failed to serialize authorship log: {}", e));
                }
            }
        }
        Err(e) => {
            debug_log(&format!(
                "Failed to read authorship log from {}: {}",
                source_sha, e
            ));
        }
    }
}

/// Emit a RevertMixed event to the rewrite log.
fn emit_revert_event(repository: &mut Repository, reverted_commit: &str, new_head: &str) {
    // Get the list of affected files from the new commit
    let affected_files = get_affected_files(repository, new_head);

    let event = RewriteLogEvent::revert_mixed(RevertMixedEvent::new(
        reverted_commit.to_string(),
        true,
        affected_files,
    ));

    match repository.storage.append_rewrite_event(event) {
        Ok(_) => debug_log("Logged RevertMixed event"),
        Err(e) => debug_log(&format!("Failed to log RevertMixed event: {}", e)),
    }
}

/// Get the list of files affected by a commit.
fn get_affected_files(repository: &Repository, commit_sha: &str) -> Vec<String> {
    let mut args = repository.global_args_for_exec();
    args.push("diff-tree".to_string());
    args.push("--no-commit-id".to_string());
    args.push("-r".to_string());
    args.push("--name-only".to_string());
    args.push(commit_sha.to_string());

    match crate::git::repository::exec_git(&args) {
        Ok(output) => String::from_utf8(output.stdout)
            .unwrap_or_default()
            .lines()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}
