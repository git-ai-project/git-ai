use crate::authorship::rebase_authorship::reconstruct_working_log_after_reset;
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::CheckpointKind;
use crate::commands::hooks::commit_hooks::get_commit_default_author;
use crate::commands::hooks::rebase_hooks;
use crate::commands::hooks::stash_hooks::{
    read_stash_authorship_note, restore_stash_attributions_from_sha,
    save_stash_authorship_log_for_sha, stash_files_for_sha,
};
use crate::error::GitAiError;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::{Repository, find_repository, find_repository_in_path};
use crate::git::rewrite_log::{
    MergeSquashEvent, RebaseCompleteEvent, ResetEvent, ResetKind, RewriteLogEvent,
};
use crate::git::sync_authorship::{fetch_authorship_notes, push_authorship_notes};
use crate::utils::{GIT_AI_SKIP_CORE_HOOKS_ENV, debug_log};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hook names that git-ai installs into `core.hooksPath`.
pub const INSTALLED_HOOKS: &[&str] = &[
    "pre-commit",
    "post-commit",
    "pre-rebase",
    "post-rewrite",
    "post-checkout",
    "post-merge",
    "pre-push",
    "reference-transaction",
    "post-index-change",
];

/// Internal file name used to preserve a user's previous global `core.hooksPath`.
pub const PREVIOUS_HOOKS_PATH_FILE: &str = "previous_hooks_path";

const CORE_HOOK_STATE_FILE: &str = "core_hook_state.json";
const STATE_EVENT_MAX_AGE_MS: u128 = 3_000;
const PENDING_PULL_AUTOSTASH_MAX_AGE_MS: u128 = 5 * 60_000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CoreHookState {
    pending_autostash: Option<PendingAutostashState>,
    pending_pull_autostash: Option<PendingPullAutostashState>,
    pending_cherry_pick: Option<PendingCherryPickState>,
    pending_stash_apply: Option<PendingStashApplyState>,
    pending_prepared_orig_head_ms: Option<u128>,
    pending_commit_base_head: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingStashApplyState {
    created_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingAutostashState {
    authorship_log_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingPullAutostashState {
    authorship_log_json: String,
    created_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingCherryPickState {
    original_head: String,
    source_commit: String,
    created_at_ms: u128,
}

pub fn handle_core_hook_command(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: git-ai hook <hook-name> [hook-args...]");
        std::process::exit(1);
    }

    if std::env::var(GIT_AI_SKIP_CORE_HOOKS_ENV).as_deref() == Ok("1") {
        std::process::exit(0);
    }

    let hook_name = &args[0];
    let hook_args = &args[1..];

    let mut repository = match find_repository_for_hook() {
        Ok(repo) => repo,
        Err(e) => {
            debug_log(&format!(
                "core hook '{}' could not find repository: {}",
                hook_name, e
            ));
            std::process::exit(0);
        }
    };

    if let Err(e) = run_hook_impl(&mut repository, hook_name, hook_args) {
        debug_log(&format!("core hook '{}' failed: {}", hook_name, e));
        // Hooks should be best-effort to avoid breaking user git workflows.
        std::process::exit(0);
    }
}

fn find_repository_for_hook() -> Result<Repository, GitAiError> {
    if let Ok(repo) = find_repository(&[]) {
        return Ok(repo);
    }

    // Some Git code paths invoke hooks with cwd set to `.git`. Recover by resolving the
    // parent worktree directory explicitly.
    if let Ok(current_dir) = std::env::current_dir()
        && current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .map(|name| name == ".git")
            .unwrap_or(false)
        && let Some(parent) = current_dir.parent()
        && let Some(parent_str) = parent.to_str()
        && let Ok(repo) = find_repository_in_path(parent_str)
    {
        return Ok(repo);
    }

    // Fallback for hook environments that provide worktree explicitly.
    if let Ok(work_tree) = std::env::var("GIT_WORK_TREE")
        && !work_tree.trim().is_empty()
        && let Ok(repo) = find_repository_in_path(work_tree.trim())
    {
        return Ok(repo);
    }

    find_repository(&[])
}

fn run_hook_impl(
    repository: &mut Repository,
    hook_name: &str,
    hook_args: &[String],
) -> Result<(), GitAiError> {
    match hook_name {
        "pre-commit" => handle_pre_commit(repository)?,
        "post-commit" => handle_post_commit(repository)?,
        "pre-rebase" => handle_pre_rebase(repository, hook_args)?,
        "post-rewrite" => handle_post_rewrite(repository, hook_args)?,
        "post-checkout" => handle_post_checkout(repository, hook_args)?,
        "post-merge" => handle_post_merge(repository, hook_args)?,
        "pre-push" => handle_pre_push(repository, hook_args)?,
        "reference-transaction" => handle_reference_transaction(repository, hook_args)?,
        "post-index-change" => handle_post_index_change(repository, hook_args)?,
        _ => {
            debug_log(&format!("unknown core hook '{}', ignoring", hook_name));
        }
    }
    Ok(())
}

fn handle_pre_commit(repository: &mut Repository) -> Result<(), GitAiError> {
    let parsed = ParsedGitInvocation {
        global_args: vec![],
        command: Some("commit".to_string()),
        command_args: vec![],
        saw_end_of_opts: false,
        is_help: false,
    };

    // Mirrors wrapper pre-commit behavior.
    let _ = crate::commands::hooks::commit_hooks::commit_pre_command_hook(&parsed, repository);

    let mut state = load_core_hook_state(repository)?;
    state.pending_commit_base_head = repository.head().ok().and_then(|h| h.target().ok());
    save_core_hook_state(repository, &state)?;
    Ok(())
}

fn handle_post_commit(repository: &mut Repository) -> Result<(), GitAiError> {
    let head_sha = match repository.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => return Ok(()),
    };

    let mut state = load_core_hook_state(repository)?;
    let original_commit = state.pending_commit_base_head.take();
    save_core_hook_state(repository, &state)?;

    let rebase_in_progress = repository.path().join("rebase-merge").exists()
        || repository.path().join("rebase-apply").exists();
    if rebase_in_progress {
        return Ok(());
    }

    let cherry_pick_head = repository.path().join("CHERRY_PICK_HEAD");
    if cherry_pick_head.exists() {
        let source_sha = repository
            .revparse_single("CHERRY_PICK_HEAD")
            .and_then(|obj| obj.peel_to_commit())
            .map(|commit| commit.id())
            .ok();
        let original_head = repository
            .find_commit(head_sha.clone())
            .ok()
            .and_then(|c| c.parent(0).ok())
            .map(|p| p.id());

        if let (Some(source_sha), Some(original_head)) = (source_sha, original_head) {
            let commit_author = get_commit_default_author(repository, &[]);
            let event = RewriteLogEvent::cherry_pick_complete(
                crate::git::rewrite_log::CherryPickCompleteEvent::new(
                    original_head,
                    head_sha.clone(),
                    vec![source_sha],
                    vec![head_sha.clone()],
                ),
            );
            repository.handle_rewrite_log_event(event, commit_author, false, true);
            return Ok(());
        }
    }

    if reflog_subject(repository)
        .as_deref()
        .map(|s| s.contains("cherry-pick"))
        .unwrap_or(false)
        && let Some(pending) = get_pending_cherry_pick_state(repository)?
    {
        let commit_author = get_commit_default_author(repository, &[]);
        let event = RewriteLogEvent::cherry_pick_complete(
            crate::git::rewrite_log::CherryPickCompleteEvent::new(
                pending.original_head,
                head_sha.clone(),
                vec![pending.source_commit],
                vec![head_sha.clone()],
            ),
        );
        repository.handle_rewrite_log_event(event, commit_author, false, true);
        clear_pending_cherry_pick_state(repository)?;
        return Ok(());
    }

    // `git commit --amend` triggers both post-commit and post-rewrite (amend).
    // Skip post-commit rewrite handling here so post-rewrite remains the single source of truth.
    let is_amend_rewrite = original_commit
        .as_ref()
        .map(|orig| !new_commit_has_parent(repository, &head_sha, orig))
        .unwrap_or(false)
        || reflog_subject(repository)
            .as_deref()
            .map(|s| s.starts_with("commit (amend):"))
            .unwrap_or(false);
    if is_amend_rewrite {
        debug_log("Skipping post-commit rewrite event for amend; waiting for post-rewrite");
        return Ok(());
    }

    // Regular commit path.
    let commit_author = get_commit_default_author(repository, &[]);
    repository.handle_rewrite_log_event(
        RewriteLogEvent::commit(original_commit, head_sha),
        commit_author,
        false,
        true,
    );
    crate::observability::spawn_background_flush();
    Ok(())
}

fn handle_pre_rebase(repository: &mut Repository, hook_args: &[String]) -> Result<(), GitAiError> {
    let parsed = ParsedGitInvocation {
        global_args: vec![],
        command: Some("rebase".to_string()),
        command_args: hook_args.to_vec(),
        saw_end_of_opts: false,
        is_help: false,
    };

    let mut context = crate::commands::git_handlers::CommandHooksContext {
        pre_commit_hook_result: None,
        rebase_original_head: None,
        rebase_onto: None,
        fetch_authorship_handle: None,
        stash_sha: None,
        push_authorship_handle: None,
        stashed_va: None,
    };
    rebase_hooks::pre_rebase_hook(&parsed, repository, &mut context);

    let mut state = load_core_hook_state(repository)?;
    // Reset stale snapshots from earlier failed rebase attempts.
    state.pending_autostash = None;
    state.pending_pull_autostash = None;

    if has_uncommitted_changes(repository)
        && let Some(old_head) = repository.head().ok().and_then(|h| h.target().ok())
        && let Ok(va) = VirtualAttributions::from_just_working_log(
            repository.clone(),
            old_head.clone(),
            Some(get_commit_default_author(repository, &parsed.command_args)),
        )
        && let Ok(authorship_log) = va.to_authorship_log()
        && let Ok(authorship_log_json) = authorship_log.serialize_to_string()
    {
        state.pending_autostash = Some(PendingAutostashState {
            authorship_log_json,
        });
        debug_log("Captured pending autostash attributions in core hook state");
    }
    save_core_hook_state(repository, &state)?;
    Ok(())
}

fn handle_post_rewrite(
    repository: &mut Repository,
    hook_args: &[String],
) -> Result<(), GitAiError> {
    let mode = hook_args
        .first()
        .map(|s| s.as_str())
        .unwrap_or_default()
        .to_string();

    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);

    let mappings: Vec<(String, String)> = stdin
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some(old), Some(new)) => Some((old.to_string(), new.to_string())),
                _ => None,
            }
        })
        .collect();

    match mode.as_str() {
        "amend" => {
            if is_rebase_in_progress(repository) || active_rebase_start_event(repository).is_some()
            {
                debug_log("Skipping post-rewrite amend handling during active rebase");
                return Ok(());
            }
            if let Some((old, new)) = mappings.first() {
                let commit_author = get_commit_default_author(repository, &[]);
                let event = RewriteLogEvent::commit_amend(old.clone(), new.clone());
                repository.handle_rewrite_log_event(event, commit_author, false, true);
            }
        }
        "rebase" => {
            let mut original_commits: Vec<String> = Vec::new();
            let mut new_commits: Vec<String> = Vec::new();
            let mut new_head = repository.head().ok().and_then(|h| h.target().ok());
            let mut is_interactive = false;

            if let Some(start_event) = latest_rebase_start_event(repository)
                && let Some(head) = new_head.clone()
            {
                let onto_for_mapping = start_event
                    .onto_head
                    .as_deref()
                    .map(str::to_string)
                    .or_else(|| resolve_rebase_onto_from_state_files(repository));
                if let Ok((mapped_original_commits, mapped_new_commits)) =
                    rebase_hooks::build_rebase_commit_mappings(
                        repository,
                        &start_event.original_head,
                        &head,
                        onto_for_mapping.as_deref(),
                    )
                {
                    original_commits = mapped_original_commits;
                    new_commits = mapped_new_commits;
                    is_interactive = start_event.is_interactive;
                }
            } else if !mappings.is_empty() {
                original_commits = mappings.iter().map(|(old, _)| old.clone()).collect();
                new_commits = mappings.iter().map(|(_, new)| new.clone()).collect();
                new_head = new_commits.last().cloned();
            }
            if !original_commits.is_empty() && !new_commits.is_empty() {
                let original_head = original_commits
                    .last()
                    .cloned()
                    .unwrap_or_else(|| original_commits[0].clone());
                let rewritten_head = new_commits
                    .last()
                    .cloned()
                    .unwrap_or_else(|| new_commits[0].clone());
                new_head = Some(rewritten_head.clone());

                let event = RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                    original_head,
                    rewritten_head,
                    is_interactive,
                    original_commits,
                    new_commits,
                ));

                let commit_author = get_commit_default_author(repository, &[]);
                repository.handle_rewrite_log_event(event, commit_author, false, true);
            }

            if let Some(new_head) = new_head {
                maybe_restore_rebase_autostash(repository, &new_head)?;
                maybe_restore_pending_pull_autostash(repository, &new_head)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn handle_post_checkout(
    repository: &mut Repository,
    hook_args: &[String],
) -> Result<(), GitAiError> {
    if hook_args.len() < 3 {
        return Ok(());
    }

    let old_head = hook_args[0].clone();
    let new_head = hook_args[1].clone();
    let branch_checkout_flag = hook_args[2].as_str() == "1";

    // Initial clone checkout: old SHA is all zeros.
    if old_head.chars().all(|c| c == '0') {
        let _ = fetch_authorship_notes(repository, "origin");
        return Ok(());
    }

    if branch_checkout_flag && old_head != new_head {
        let _ = repository.storage.rename_working_log(&old_head, &new_head);
        let _ = trim_working_log_to_current_changes(repository, &new_head);
    } else if !branch_checkout_flag && old_head == new_head {
        let _ = trim_working_log_to_current_changes(repository, &old_head);
    }
    Ok(())
}

fn handle_post_merge(repository: &mut Repository, hook_args: &[String]) -> Result<(), GitAiError> {
    let mut parsed = ParsedGitInvocation {
        global_args: vec![],
        command: Some("merge".to_string()),
        command_args: vec![],
        saw_end_of_opts: false,
        is_help: false,
    };

    if hook_args.first().map(|s| s.as_str()) == Some("1") {
        parsed.command_args.push("--squash".to_string());
        prepare_merge_squash_from_post_merge(repository);
    }

    if reflog_subject(repository)
        .as_deref()
        .map(|subject| subject.starts_with("pull"))
        .unwrap_or(false)
    {
        let old_head = repository
            .revparse_single("ORIG_HEAD")
            .and_then(|obj| obj.peel_to_commit())
            .map(|c| c.id())
            .ok();
        let new_head = repository.head().ok().and_then(|h| h.target().ok());
        if let (Some(old), Some(new)) = (old_head, new_head)
            && old != new
        {
            let _ = repository.storage.rename_working_log(&old, &new);
            let _ = maybe_restore_pending_pull_autostash(repository, &new);
        }
    }

    Ok(())
}

fn handle_pre_push(repository: &Repository, hook_args: &[String]) -> Result<(), GitAiError> {
    if let Some(remote_name) = hook_args.first() {
        let _ = push_authorship_notes(repository, remote_name);
    }
    Ok(())
}

fn handle_reference_transaction(
    repository: &mut Repository,
    hook_args: &[String],
) -> Result<(), GitAiError> {
    let stage = hook_args.first().map(|s| s.as_str()).unwrap_or_default();
    if stage != "prepared" && stage != "committed" {
        return Ok(());
    }

    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    if stdin.trim().is_empty() {
        return Ok(());
    }

    let mut remotes_to_sync: HashSet<String> = HashSet::new();
    let mut saw_orig_head_update = false;
    let mut moved_branch_ref: Option<(String, String)> = None;
    let mut moved_head_ref: Option<(String, String)> = None;
    let mut created_stash_sha: Option<String> = None;
    let mut deleted_stash_sha: Option<String> = None;
    let mut created_cherry_pick_head: Option<String> = None;
    let mut deleted_cherry_pick_head: Option<String> = None;
    let mut created_auto_merge_sha: Option<String> = None;

    for line in stdin.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let old = parts[0];
        let new = parts[1];
        let reference = parts[2];

        if reference == "ORIG_HEAD" && old != new {
            saw_orig_head_update = true;
        }

        if reference.starts_with("refs/remotes/")
            && old != new
            && let Some(remote) = reference
                .strip_prefix("refs/remotes/")
                .and_then(|r| r.split('/').next())
            && !remote.is_empty()
        {
            remotes_to_sync.insert(remote.to_string());
        }

        if reference.starts_with("refs/heads/") && old != new {
            moved_branch_ref = Some((old.to_string(), new.to_string()));
        }

        if reference == "HEAD" && old != new {
            moved_head_ref = Some((old.to_string(), new.to_string()));
        }

        if reference == "refs/stash" {
            if is_zero_oid(old) && !is_zero_oid(new) {
                created_stash_sha = Some(new.to_string());
            } else if !is_zero_oid(old) && is_zero_oid(new) {
                deleted_stash_sha = Some(old.to_string());
            }
        }

        if reference == "CHERRY_PICK_HEAD" {
            if is_zero_oid(old) && !is_zero_oid(new) {
                created_cherry_pick_head = Some(new.to_string());
            } else if !is_zero_oid(old) && is_zero_oid(new) {
                deleted_cherry_pick_head = Some(old.to_string());
            }
        }

        if reference == "AUTO_MERGE" && is_zero_oid(old) && !is_zero_oid(new) {
            created_auto_merge_sha = Some(new.to_string());
        }
    }

    // Prefer concrete branch ref updates, but fall back to detached-HEAD updates.
    let moved_main_ref = moved_branch_ref.or(moved_head_ref);

    if stage == "prepared" {
        let mut state = load_core_hook_state(repository)?;
        if saw_orig_head_update {
            state.pending_prepared_orig_head_ms = Some(now_ms());
            if reflog_action()
                .as_deref()
                .map(|action| action.starts_with("pull --rebase"))
                .unwrap_or(false)
            {
                capture_pending_pull_autostash_state(repository, &mut state);
            }
        }

        let has_recent_orig_head = state
            .pending_prepared_orig_head_ms
            .map(|ts| now_ms().saturating_sub(ts) <= STATE_EVENT_MAX_AGE_MS)
            .unwrap_or(false);

        if has_recent_orig_head
            && let Some((_, target_head)) = moved_main_ref.as_ref()
            && !is_rebase_in_progress(repository)
        {
            capture_pre_reset_state(repository, target_head);
            state.pending_prepared_orig_head_ms = None;
        }

        if let Some(ts) = state.pending_prepared_orig_head_ms
            && now_ms().saturating_sub(ts) > STATE_EVENT_MAX_AGE_MS
        {
            state.pending_prepared_orig_head_ms = None;
        }

        // Drop stale pull-autostash snapshots that never got restored.
        if let Some(pending) = state.pending_pull_autostash.as_ref()
            && now_ms().saturating_sub(pending.created_at_ms) > PENDING_PULL_AUTOSTASH_MAX_AGE_MS
        {
            state.pending_pull_autostash = None;
        }
        save_core_hook_state(repository, &state)?;
        return Ok(());
    }

    for remote in remotes_to_sync {
        let _ = fetch_authorship_notes(repository, &remote);
    }

    if let Some(stash_sha) = created_stash_sha {
        let _ = handle_stash_created(repository, &stash_sha);
    }

    if let Some(stash_sha) = deleted_stash_sha {
        let _ = restore_stash_attributions_from_sha(repository, &stash_sha);
        clear_pending_stash_apply(repository)?;
    }

    if created_auto_merge_sha.is_some() {
        mark_pending_stash_apply(repository)?;
    }

    if let Some(source_commit) = created_cherry_pick_head {
        let _ = set_pending_cherry_pick_state(repository, &source_commit);
    }

    let reflog = reflog_subject(repository);

    if deleted_cherry_pick_head.is_some()
        && reflog
            .as_deref()
            .map(|s| s.contains("cherry-pick") && s.contains("abort"))
            .unwrap_or(false)
    {
        let _ = clear_pending_cherry_pick_state(repository);
    }

    // Track reset operations from reflog instead of command env.
    if let Some((old_head, new_head)) = moved_main_ref
        && !is_rebase_in_progress(repository)
        && reflog
            .as_deref()
            .map(|s| s.starts_with("reset:"))
            .unwrap_or(false)
    {
        let mode = detect_reset_mode_from_worktree(repository);
        let _ = apply_reset_side_effects(repository, &old_head, &new_head, mode);
    }

    if reflog
        .as_deref()
        .map(|s| s.starts_with("pull --rebase (finish):"))
        .unwrap_or(false)
        && let Some(start_event) = active_rebase_start_event(repository)
    {
        process_rebase_completion_from_start(repository, start_event);
    }

    if reflog
        .as_deref()
        .map(|s| s.starts_with("pull --rebase (finish):"))
        .unwrap_or(false)
        && let Some(new_head) = repository.head().ok().and_then(|h| h.target().ok())
    {
        let _ = maybe_restore_pending_pull_autostash(repository, &new_head);
    }

    Ok(())
}

fn handle_post_index_change(
    repository: &mut Repository,
    hook_args: &[String],
) -> Result<(), GitAiError> {
    let _ = hook_args;
    let _ = maybe_restore_stash_apply_without_pop(repository);
    Ok(())
}

fn apply_reset_side_effects(
    repository: &mut Repository,
    old_head: &str,
    new_head: &str,
    mode: ResetKind,
) -> Result<(), GitAiError> {
    let human_author = get_commit_default_author(repository, &[]);

    match mode {
        ResetKind::Hard => {
            let _ = repository
                .storage
                .delete_working_log_for_base_commit(old_head);
        }
        ResetKind::Soft | ResetKind::Mixed => {
            // Backward reset reconstruction: preserve AI attributions for unwound commits.
            if is_ancestor(repository, new_head, old_head) {
                let _ = reconstruct_working_log_after_reset(
                    repository,
                    new_head,
                    old_head,
                    &human_author,
                    None,
                );
            }
        }
    }

    let _ = repository
        .storage
        .append_rewrite_event(RewriteLogEvent::Reset {
            reset: ResetEvent::new(
                mode,
                false,
                false,
                new_head.to_string(),
                old_head.to_string(),
            ),
        });
    Ok(())
}

fn maybe_restore_rebase_autostash(
    repository: &mut Repository,
    new_head: &str,
) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    if let Some(pending) = state.pending_autostash.clone() {
        debug_log("Restoring pending autostash attributions in core hooks");
        if let Ok(authorship_log) =
            crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(
                &pending.authorship_log_json,
            )
        {
            apply_initial_attributions_from_authorship_log(repository, new_head, &authorship_log);
        }
        state.pending_autostash = None;
        save_core_hook_state(repository, &state)?;
    }
    Ok(())
}

fn maybe_restore_pending_pull_autostash(
    repository: &mut Repository,
    new_head: &str,
) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    let Some(pending) = state.pending_pull_autostash.clone() else {
        return Ok(());
    };

    if now_ms().saturating_sub(pending.created_at_ms) > PENDING_PULL_AUTOSTASH_MAX_AGE_MS {
        state.pending_pull_autostash = None;
        save_core_hook_state(repository, &state)?;
        return Ok(());
    }

    debug_log("Restoring pending pull-autostash attributions in core hooks");
    if let Ok(authorship_log) =
        crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(
            &pending.authorship_log_json,
        )
    {
        apply_initial_attributions_from_authorship_log(repository, new_head, &authorship_log);
    }
    state.pending_pull_autostash = None;
    save_core_hook_state(repository, &state)?;
    Ok(())
}

fn is_zero_oid(oid: &str) -> bool {
    !oid.is_empty() && oid.chars().all(|c| c == '0')
}

fn reflog_subject(repository: &Repository) -> Option<String> {
    let mut args = repository.global_args_for_exec();
    args.push("reflog".to_string());
    args.push("-1".to_string());
    args.push("--format=%gs".to_string());

    let output = crate::git::repository::exec_git(&args).ok()?;
    if !output.status.success() {
        return None;
    }
    let subject = String::from_utf8(output.stdout).ok()?;
    let subject = subject.trim().to_string();
    if subject.is_empty() {
        None
    } else {
        Some(subject)
    }
}

fn reflog_action() -> Option<String> {
    std::env::var("GIT_REFLOG_ACTION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn handle_stash_created(repository: &Repository, stash_sha: &str) -> Result<(), GitAiError> {
    let head_sha = match repository.head().ok().and_then(|h| h.target().ok()) {
        Some(sha) => sha,
        None => return Ok(()),
    };
    let stash_files = stash_files_for_sha(repository, stash_sha).unwrap_or_default();
    if stash_files.is_empty() {
        return Ok(());
    }
    save_stash_authorship_log_for_sha(repository, &head_sha, stash_sha, &stash_files)
}

fn mark_pending_stash_apply(repository: &Repository) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    state.pending_stash_apply = Some(PendingStashApplyState {
        created_at_ms: now_ms(),
    });
    save_core_hook_state(repository, &state)
}

fn clear_pending_stash_apply(repository: &Repository) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    state.pending_stash_apply = None;
    save_core_hook_state(repository, &state)
}

fn maybe_restore_stash_apply_without_pop(repository: &Repository) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    let Some(pending) = state.pending_stash_apply.clone() else {
        return Ok(());
    };

    if now_ms().saturating_sub(pending.created_at_ms) > STATE_EVENT_MAX_AGE_MS {
        state.pending_stash_apply = None;
        save_core_hook_state(repository, &state)?;
        return Ok(());
    }

    let Some(candidate) = find_best_matching_stash_with_note(repository)? else {
        return Ok(());
    };

    let _ = restore_stash_attributions_from_sha(repository, &candidate);
    state.pending_stash_apply = None;
    save_core_hook_state(repository, &state)
}

fn find_best_matching_stash_with_note(
    repository: &Repository,
) -> Result<Option<String>, GitAiError> {
    let changed_files: HashSet<String> = repository
        .get_staged_and_unstaged_filenames()
        .unwrap_or_default()
        .into_iter()
        .collect();
    if changed_files.is_empty() {
        return Ok(None);
    }

    let stash_shas = list_stash_shas(repository)?;
    let mut best: Option<(usize, usize, String)> = None;

    for stash_sha in stash_shas {
        let note_content = match read_stash_authorship_note(repository, &stash_sha) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let note_files = note_files_from_authorship_note(&note_content);
        if note_files.is_empty() {
            continue;
        }

        let match_count = note_files
            .iter()
            .filter(|file| changed_files.contains(*file))
            .count();
        if match_count == 0 {
            continue;
        }

        let candidate = (match_count, note_files.len(), stash_sha);
        let is_better = best
            .as_ref()
            .map(|(best_match_count, best_total_files, _)| {
                candidate.0 > *best_match_count
                    || (candidate.0 == *best_match_count && candidate.1 < *best_total_files)
            })
            .unwrap_or(true);
        if is_better {
            best = Some(candidate);
        }
    }

    Ok(best.map(|(_, _, stash_sha)| stash_sha))
}

fn note_files_from_authorship_note(content: &str) -> Vec<String> {
    crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(content)
        .map(|log| {
            log.attestations
                .into_iter()
                .map(|a| a.file_path)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn list_stash_shas(repository: &Repository) -> Result<Vec<String>, GitAiError> {
    let mut args = repository.global_args_for_exec();
    args.push("stash".to_string());
    args.push("list".to_string());
    args.push("--format=%H".to_string());

    let output = crate::git::repository::exec_git(&args)?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn set_pending_cherry_pick_state(
    repository: &Repository,
    source_commit: &str,
) -> Result<(), GitAiError> {
    let Some(original_head) = repository.head().ok().and_then(|h| h.target().ok()) else {
        return Ok(());
    };
    let mut state = load_core_hook_state(repository)?;
    state.pending_cherry_pick = Some(PendingCherryPickState {
        original_head,
        source_commit: source_commit.to_string(),
        created_at_ms: now_ms(),
    });
    save_core_hook_state(repository, &state)
}

fn get_pending_cherry_pick_state(
    repository: &Repository,
) -> Result<Option<PendingCherryPickState>, GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    if let Some(pending) = state.pending_cherry_pick.as_ref()
        && now_ms().saturating_sub(pending.created_at_ms) > PENDING_PULL_AUTOSTASH_MAX_AGE_MS
    {
        state.pending_cherry_pick = None;
        save_core_hook_state(repository, &state)?;
        return Ok(None);
    }
    Ok(state.pending_cherry_pick.clone())
}

fn clear_pending_cherry_pick_state(repository: &Repository) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    state.pending_cherry_pick = None;
    save_core_hook_state(repository, &state)
}

fn trim_working_log_to_current_changes(
    repository: &Repository,
    base_commit: &str,
) -> Result<(), GitAiError> {
    let changed_files: HashSet<String> = repository
        .get_staged_and_unstaged_filenames()
        .unwrap_or_default()
        .into_iter()
        .collect();

    let working_log = repository.storage.working_log_for_base_commit(base_commit);
    let initial = working_log.read_initial_attributions();
    let filtered_initial_files: std::collections::HashMap<_, _> = initial
        .files
        .into_iter()
        .filter(|(file, _)| changed_files.contains(file))
        .collect();
    working_log.write_initial_attributions(filtered_initial_files, initial.prompts)?;

    let checkpoints = working_log.read_all_checkpoints().unwrap_or_default();
    let filtered_checkpoints: Vec<_> = checkpoints
        .into_iter()
        .map(|mut checkpoint| {
            checkpoint
                .entries
                .retain(|entry| changed_files.contains(&entry.file));
            checkpoint
        })
        .filter(|checkpoint| !checkpoint.entries.is_empty())
        .collect();
    working_log.write_all_checkpoints(&filtered_checkpoints)?;
    Ok(())
}

fn latest_rebase_start_event(
    repository: &Repository,
) -> Option<crate::git::rewrite_log::RebaseStartEvent> {
    let events = repository.storage.read_rewrite_events().ok()?;
    for event in events {
        if let RewriteLogEvent::RebaseStart { rebase_start } = event {
            return Some(rebase_start);
        }
    }
    None
}

fn active_rebase_start_event(
    repository: &Repository,
) -> Option<crate::git::rewrite_log::RebaseStartEvent> {
    let events = repository.storage.read_rewrite_events().ok()?;
    for event in events {
        match event {
            RewriteLogEvent::RebaseComplete { .. } | RewriteLogEvent::RebaseAbort { .. } => {
                return None;
            }
            RewriteLogEvent::RebaseStart { rebase_start } => return Some(rebase_start),
            _ => continue,
        }
    }
    None
}

fn process_rebase_completion_from_start(
    repository: &mut Repository,
    start_event: crate::git::rewrite_log::RebaseStartEvent,
) {
    let Some(new_head) = repository.head().ok().and_then(|h| h.target().ok()) else {
        return;
    };

    let (original_commits, new_commits) = match rebase_hooks::build_rebase_commit_mappings(
        repository,
        &start_event.original_head,
        &new_head,
        start_event.onto_head.as_deref(),
    ) {
        Ok(mappings) => mappings,
        Err(_) => {
            let _ = maybe_restore_rebase_autostash(repository, &new_head);
            return;
        }
    };

    if !original_commits.is_empty() && !new_commits.is_empty() {
        let event = RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
            start_event.original_head,
            new_head.clone(),
            false,
            original_commits,
            new_commits,
        ));
        let commit_author = get_commit_default_author(repository, &[]);
        repository.handle_rewrite_log_event(event, commit_author, false, true);
    }

    let _ = maybe_restore_rebase_autostash(repository, &new_head);
}

fn detect_reset_mode_from_worktree(repository: &Repository) -> ResetKind {
    let entries = repository.status(None, false).unwrap_or_default();

    let has_staged_changes = entries.iter().any(|entry| {
        entry.staged != crate::git::status::StatusCode::Unmodified
            && entry.staged != crate::git::status::StatusCode::Ignored
    });
    let has_unstaged_changes = entries.iter().any(|entry| {
        entry.unstaged != crate::git::status::StatusCode::Unmodified
            && entry.unstaged != crate::git::status::StatusCode::Ignored
    });

    if has_staged_changes {
        ResetKind::Soft
    } else if has_unstaged_changes {
        ResetKind::Mixed
    } else {
        ResetKind::Hard
    }
}

fn capture_pre_reset_state(repository: &mut Repository, target_head: &str) {
    let human_author = get_commit_default_author(repository, &[]);
    let _ = crate::commands::checkpoint::run(
        repository,
        &human_author,
        CheckpointKind::Human,
        false,
        false,
        true,
        None,
        true,
    );
    repository.require_pre_command_head();
    repository.pre_reset_target_commit = Some(target_head.to_string());
}

fn capture_pending_pull_autostash_state(repository: &Repository, state: &mut CoreHookState) {
    let Some(head_sha) = repository.head().ok().and_then(|h| h.target().ok()) else {
        return;
    };

    let human_author = get_commit_default_author(repository, &[]);
    let Ok(va) = VirtualAttributions::from_just_working_log(
        repository.clone(),
        head_sha,
        Some(human_author),
    ) else {
        return;
    };
    if va.attributions.is_empty() {
        return;
    }

    let Ok(authorship_log) = va.to_authorship_log() else {
        return;
    };
    if authorship_log.attestations.is_empty() {
        return;
    }

    let Ok(authorship_log_json) = authorship_log.serialize_to_string() else {
        return;
    };

    state.pending_pull_autostash = Some(PendingPullAutostashState {
        authorship_log_json,
        created_at_ms: now_ms(),
    });
    debug_log("Captured pending pull-autostash attributions in core hook state");
}

fn apply_initial_attributions_from_authorship_log(
    repository: &Repository,
    base_commit: &str,
    authorship_log: &crate::authorship::authorship_log_serialization::AuthorshipLog,
) {
    let mut initial_files = HashMap::new();

    for attestation in &authorship_log.attestations {
        let mut line_attrs = Vec::new();
        for entry in &attestation.entries {
            for range in &entry.line_ranges {
                let (start, end) = match range {
                    crate::authorship::authorship_log::LineRange::Single(line) => (*line, *line),
                    crate::authorship::authorship_log::LineRange::Range(start, end) => {
                        (*start, *end)
                    }
                };
                line_attrs.push(crate::authorship::attribution_tracker::LineAttribution {
                    start_line: start,
                    end_line: end,
                    author_id: entry.hash.clone(),
                    overrode: None,
                });
            }
        }
        if !line_attrs.is_empty() {
            initial_files.insert(attestation.file_path.clone(), line_attrs);
        }
    }

    let initial_prompts: HashMap<_, _> = authorship_log
        .metadata
        .prompts
        .clone()
        .into_iter()
        .collect();
    let working_log = repository.storage.working_log_for_base_commit(base_commit);

    let existing_initial = working_log.read_initial_attributions();
    let mut merged_files = existing_initial.files;
    for (file, attrs) in initial_files {
        merged_files.insert(file, attrs);
    }
    let mut merged_prompts = existing_initial.prompts;
    for (prompt_id, prompt) in initial_prompts {
        merged_prompts.insert(prompt_id, prompt);
    }

    let _ = working_log.write_initial_attributions(merged_files, merged_prompts);
}

fn prepare_merge_squash_from_post_merge(repository: &mut Repository) {
    let Some(action) = std::env::var("GIT_REFLOG_ACTION").ok() else {
        return;
    };
    let Some(source_ref) = parse_merge_source_ref_from_reflog_action(&action) else {
        return;
    };

    let source_head = match repository
        .revparse_single(&source_ref)
        .and_then(|obj| obj.peel_to_commit())
        .map(|commit| commit.id())
    {
        Ok(sha) => sha,
        Err(_) => return,
    };

    let base_ref = match repository.head() {
        Ok(head) => head,
        Err(_) => return,
    };
    let base_head = match base_ref.target() {
        Ok(sha) => sha,
        Err(_) => return,
    };
    let base_branch = base_ref.name().unwrap_or("HEAD").to_string();
    let commit_author = get_commit_default_author(repository, &[]);

    let event = RewriteLogEvent::merge_squash(MergeSquashEvent::new(
        source_ref,
        source_head,
        base_branch,
        base_head,
    ));
    repository.handle_rewrite_log_event(event, commit_author, false, true);
}

fn parse_merge_source_ref_from_reflog_action(action: &str) -> Option<String> {
    let tokens: Vec<&str> = action.split_whitespace().collect();
    if tokens.first().copied() != Some("merge") {
        return None;
    }

    tokens
        .into_iter()
        .rev()
        .find(|token| !token.starts_with('-') && *token != "merge")
        .map(ToOwned::to_owned)
}

fn has_uncommitted_changes(repository: &Repository) -> bool {
    repository
        .get_staged_and_unstaged_filenames()
        .map(|files| !files.is_empty())
        .unwrap_or(false)
}

fn is_rebase_in_progress(repository: &Repository) -> bool {
    repository.path().join("rebase-merge").exists()
        || repository.path().join("rebase-apply").exists()
}

fn resolve_rebase_onto_from_state_files(repository: &Repository) -> Option<String> {
    let candidates = [
        repository.path().join("rebase-merge").join("onto"),
        repository.path().join("rebase-apply").join("onto"),
    ];
    for path in candidates {
        if let Ok(content) = fs::read_to_string(&path) {
            let onto = content.trim();
            if !onto.is_empty() {
                return Some(onto.to_string());
            }
        }
    }
    None
}

fn is_ancestor(repository: &Repository, ancestor: &str, descendant: &str) -> bool {
    let mut args = repository.global_args_for_exec();
    args.push("merge-base".to_string());
    args.push("--is-ancestor".to_string());
    args.push(ancestor.to_string());
    args.push(descendant.to_string());
    crate::git::repository::exec_git(&args).is_ok()
}

fn new_commit_has_parent(repository: &Repository, new_commit: &str, expected_parent: &str) -> bool {
    repository
        .find_commit(new_commit.to_string())
        .ok()
        .and_then(|commit| commit.parent(0).ok())
        .map(|parent| parent.id() == expected_parent)
        .unwrap_or(false)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn load_core_hook_state(repository: &Repository) -> Result<CoreHookState, GitAiError> {
    let path = core_hook_state_path(repository);
    if !path.exists() {
        return Ok(CoreHookState::default());
    }
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

fn save_core_hook_state(repository: &Repository, state: &CoreHookState) -> Result<(), GitAiError> {
    let path = core_hook_state_path(repository);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string(state)?)?;
    Ok(())
}

fn core_hook_state_path(repository: &Repository) -> PathBuf {
    repository.path().join("ai").join(CORE_HOOK_STATE_FILE)
}

/// Returns the managed global core-hooks directory.
pub fn managed_core_hooks_dir() -> Result<PathBuf, GitAiError> {
    let home = dirs::home_dir()
        .ok_or_else(|| GitAiError::Generic("Unable to determine home directory".to_string()))?;
    Ok(home.join(".git-ai").join("core-hooks"))
}

/// Writes git hook shims that dispatch to `git-ai hook <hook-name>`.
pub fn write_core_hook_scripts(hooks_dir: &Path, git_ai_binary: &Path) -> Result<(), GitAiError> {
    fs::create_dir_all(hooks_dir)?;
    let binary = git_ai_binary
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");

    for hook in INSTALLED_HOOKS {
        let script = format!(
            "#!/bin/sh\nif [ \"${{{env}:-}}\" = \"1\" ]; then\n  exit 0\nfi\nexec \"{bin}\" hook {hook} \"$@\"\n",
            env = GIT_AI_SKIP_CORE_HOOKS_ENV,
            bin = binary,
            hook = hook,
        );
        let hook_path = hooks_dir.join(hook);
        fs::write(&hook_path, script)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::find_repository_for_hook;
    use crate::git::test_utils::TmpRepo;
    use serial_test::serial;

    #[test]
    #[serial]
    fn find_repository_for_hook_recovers_from_git_dir_cwd() {
        struct CwdGuard(std::path::PathBuf);
        impl Drop for CwdGuard {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }

        let repo = TmpRepo::new().expect("tmp repo");
        let original_cwd = std::env::current_dir().expect("cwd");
        let _guard = CwdGuard(original_cwd);
        let git_dir = repo.path().join(".git");

        std::env::set_current_dir(&git_dir).expect("set cwd to .git");
        let resolved = find_repository_for_hook().expect("resolve repository from .git cwd");
        let resolved_workdir = resolved.workdir().expect("workdir");
        let resolved_canonical = resolved_workdir.canonicalize().expect("canonical workdir");
        let expected_canonical = repo.path().canonicalize().expect("canonical expected");

        assert_eq!(resolved_canonical, expected_canonical);
    }
}
