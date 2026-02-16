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
use crate::git::repository::{
    Repository, find_repository, find_repository_from_hook_env, find_repository_in_path,
};
use crate::git::rewrite_log::{
    MergeSquashEvent, RebaseCompleteEvent, ResetEvent, ResetKind, RewriteLogEvent,
};
use crate::git::sync_authorship::{fetch_authorship_notes, push_authorship_notes};
use crate::utils::{
    GIT_AI_GIT_CMD_ENV, GIT_AI_SKIP_CORE_HOOKS_ENV, GIT_AI_TRAMPOLINE_SKIP_CHAIN_ENV, debug_log,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hook names that git-ai installs into `core.hooksPath`.
///
/// We install passthrough shims for standard client hooks (even when git-ai has no
/// custom behavior) so existing user hook ecosystems like Husky continue to run.
pub const INSTALLED_HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-rewrite",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "reference-transaction",
    "post-index-change",
];

/// Hooks that only need shell-level chaining and no internal git-ai handler.
pub const PASSTHROUGH_ONLY_HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "pre-auto-gc",
];

/// Internal file name used to preserve a user's previous global `core.hooksPath`.
pub const PREVIOUS_HOOKS_PATH_FILE: &str = "previous_hooks_path";
pub const PENDING_STASH_APPLY_MARKER_FILE: &str = "pending_stash_apply";
pub const WORKING_LOG_INITIAL_FILE: &str = "INITIAL";

const CORE_HOOK_STATE_FILE: &str = "core_hook_state.json";
const STATE_EVENT_MAX_AGE_MS: u128 = 3_000;
const PENDING_PULL_AUTOSTASH_MAX_AGE_MS: u128 = 5 * 60_000;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CoreHookState {
    pending_autostash: Option<PendingAutostashState>,
    pending_pull_autostash: Option<PendingPullAutostashState>,
    pending_cherry_pick: Option<PendingCherryPickState>,
    pending_stash_apply: Option<PendingStashApplyState>,
    pending_stash_ref_update: Option<PendingStashRefUpdateState>,
    pending_prepared_orig_head_ms: Option<u128>,
    pending_commit_base_head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingStashApplyState {
    created_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingStashRefUpdateState {
    created_at_ms: u128,
    stash_count_before: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingAutostashState {
    authorship_log_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingPullAutostashState {
    authorship_log_json: String,
    created_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingCherryPickState {
    original_head: String,
    source_commit: String,
    created_at_ms: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefTxnActionClass {
    Unknown,
    CommitLike,
    ResetLike,
    RebaseLike,
    PullRebaseLike,
    StashLike,
    CherryPickLike,
}

#[derive(Debug, Default)]
struct HookInvocationCache {
    head_reflog_entry: Option<Option<HeadReflogEntry>>,
    reflog_subject: Option<Option<String>>,
    head_sha: Option<Option<String>>,
    stash_count: Option<Option<usize>>,
}

#[derive(Debug, Clone)]
struct HeadReflogEntry {
    old_sha: String,
    new_sha: String,
    subject: String,
}

impl HookInvocationCache {
    fn head_reflog_entry(&mut self, repository: &Repository) -> Option<HeadReflogEntry> {
        if self.head_reflog_entry.is_none() {
            self.head_reflog_entry = Some(read_last_head_reflog_entry(repository));
        }
        self.head_reflog_entry.clone().flatten()
    }

    fn reflog_subject(&mut self, repository: &Repository) -> Option<String> {
        if self.reflog_subject.is_none() {
            let subject = self
                .head_reflog_entry(repository)
                .map(|entry| entry.subject)
                .or_else(|| reflog_subject(repository));
            self.reflog_subject = Some(subject);
        }
        self.reflog_subject.clone().flatten()
    }

    fn head_sha(&mut self, repository: &Repository) -> Option<String> {
        if self.head_sha.is_none() {
            let head_sha = self
                .head_reflog_entry(repository)
                .map(|entry| entry.new_sha)
                .or_else(|| rev_parse(repository, "HEAD"));
            self.head_sha = Some(head_sha);
        }
        self.head_sha.clone().flatten()
    }

    fn stash_count(&mut self, repository: &Repository) -> Option<usize> {
        *self
            .stash_count
            .get_or_insert_with(|| stash_entry_count(repository))
    }
}

pub fn handle_core_hook_command(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: git-ai hook <hook-name> [hook-args...]");
        std::process::exit(1);
    }

    let hook_name = &args[0];
    let hook_args = &args[1..];
    run_core_hook_best_effort(hook_name, hook_args, None);
}

pub fn run_core_hook_best_effort(
    hook_name: &str,
    hook_args: &[String],
    stdin_override: Option<&str>,
) {
    if std::env::var(GIT_AI_SKIP_CORE_HOOKS_ENV).as_deref() == Ok("1") {
        return;
    }

    let mut repository = match find_repository_for_hook() {
        Ok(repo) => repo,
        Err(e) => {
            debug_log(&format!(
                "core hook '{}' could not find repository: {}",
                hook_name, e
            ));
            return;
        }
    };

    if let Err(e) = run_hook_impl(&mut repository, hook_name, hook_args, stdin_override) {
        debug_log(&format!("core hook '{}' failed: {}", hook_name, e));
        // Hooks should be best-effort to avoid breaking user git workflows.
    }
}

fn find_repository_for_hook() -> Result<Repository, GitAiError> {
    if let Ok(repo) = find_repository_from_hook_env() {
        return Ok(repo);
    }

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
    stdin_override: Option<&str>,
) -> Result<(), GitAiError> {
    if PASSTHROUGH_ONLY_HOOKS.contains(&hook_name) {
        return Ok(());
    }

    match hook_name {
        "pre-commit" => handle_pre_commit(repository)?,
        "post-commit" => handle_post_commit(repository)?,
        "pre-rebase" => handle_pre_rebase(repository, hook_args)?,
        "post-rewrite" => handle_post_rewrite(repository, hook_args, stdin_override)?,
        "post-checkout" => handle_post_checkout(repository, hook_args)?,
        "post-merge" => handle_post_merge(repository, hook_args)?,
        "pre-push" => handle_pre_push(repository, hook_args)?,
        "reference-transaction" => {
            handle_reference_transaction(repository, hook_args, stdin_override)?
        }
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
    Ok(())
}

fn handle_post_commit(repository: &mut Repository) -> Result<(), GitAiError> {
    let mut cache = HookInvocationCache::default();
    let reflog_entry = cache.head_reflog_entry(repository);
    let previous_head_from_reflog = reflog_entry
        .as_ref()
        .and_then(|entry| non_zero_oid(entry.old_sha.as_str()));
    let head_sha = match cache.head_sha(repository) {
        Some(sha) => sha,
        None => return Ok(()),
    };

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
        let original_head = previous_head_from_reflog
            .clone()
            .or_else(|| first_parent_of_commit(repository, &head_sha));

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

    let reflog = cache.reflog_subject(repository);
    if reflog
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

    if !has_non_empty_working_logs(repository) && !has_ai_notes_ref(repository) {
        return Ok(());
    }

    // `git commit --amend` triggers both post-commit and post-rewrite (amend).
    // Skip post-commit rewrite handling here so post-rewrite remains the single source of truth.
    let first_parent = first_parent_of_commit(repository, &head_sha);
    let is_amend_rewrite = previous_head_from_reflog
        .as_ref()
        .zip(first_parent.as_ref())
        .map(|(old_head, parent)| old_head != parent)
        .unwrap_or(false)
        || reflog
            .as_deref()
            .map(|s| s.starts_with("commit (amend):"))
            .unwrap_or(false);
    if is_amend_rewrite {
        debug_log("Skipping post-commit rewrite event for amend; waiting for post-rewrite");
        return Ok(());
    }

    // Regular commit path.
    let original_commit = previous_head_from_reflog.or(first_parent);
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
    let before = state.clone();
    // Reset stale snapshots from earlier failed rebase attempts.
    state.pending_autostash = None;
    if let Some(pending) = state.pending_pull_autostash.as_ref()
        && now_ms().saturating_sub(pending.created_at_ms) > PENDING_PULL_AUTOSTASH_MAX_AGE_MS
    {
        state.pending_pull_autostash = None;
    }

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
    save_core_hook_state_if_changed(repository, &before, &state)?;
    Ok(())
}

fn handle_post_rewrite(
    repository: &mut Repository,
    hook_args: &[String],
    stdin_override: Option<&str>,
) -> Result<(), GitAiError> {
    let mode = hook_args
        .first()
        .map(|s| s.as_str())
        .unwrap_or_default()
        .to_string();

    let stdin_storage = if stdin_override.is_none() {
        let mut stdin = String::new();
        let _ = std::io::stdin().read_to_string(&mut stdin);
        Some(stdin)
    } else {
        None
    };
    let stdin = stdin_override.unwrap_or_else(|| stdin_storage.as_deref().unwrap_or_default());

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

            if let Some(start_event) = active_rebase_start_event(repository)
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
    if is_zero_oid(&old_head) {
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
    stdin_override: Option<&str>,
) -> Result<(), GitAiError> {
    let stage = hook_args.first().map(|s| s.as_str()).unwrap_or_default();
    if stage != "prepared" && stage != "committed" {
        return Ok(());
    }

    let stdin_storage = if stdin_override.is_none() {
        let mut stdin = String::new();
        let _ = std::io::stdin().read_to_string(&mut stdin);
        Some(stdin)
    } else {
        None
    };
    let stdin = stdin_override.unwrap_or_else(|| stdin_storage.as_deref().unwrap_or_default());
    if stdin.trim().is_empty() {
        return Ok(());
    }

    let mut remotes_to_sync: HashSet<String> = HashSet::new();
    let mut saw_orig_head_update = false;
    let mut moved_branch_ref: Option<(String, String)> = None;
    let mut moved_head_ref: Option<(String, String)> = None;
    let mut stash_ref_update: Option<(String, String)> = None;
    let mut created_cherry_pick_head: Option<String> = None;
    let mut deleted_cherry_pick_head: Option<String> = None;
    let mut created_auto_merge_sha: Option<String> = None;

    for line in stdin.lines() {
        let Some((old, new, reference)) = parse_reference_transaction_line(line) else {
            continue;
        };

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

        if reference == "refs/stash" && old != new {
            stash_ref_update = Some((old.to_string(), new.to_string()));
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
    let reflog_action_value = reflog_action();
    let action_class = classify_ref_transaction_action(reflog_action_value.as_deref());
    let stash_transition_kind = stash_ref_update
        .as_ref()
        .map(|(old, new)| classify_stash_ref_transition_kind(old, new));

    // Fast no-op path: skip state churn and extra git commands when nothing relevant changed.
    if !saw_orig_head_update
        && remotes_to_sync.is_empty()
        && moved_main_ref.is_none()
        && stash_ref_update.is_none()
        && created_cherry_pick_head.is_none()
        && deleted_cherry_pick_head.is_none()
        && created_auto_merge_sha.is_none()
    {
        return Ok(());
    }

    let may_prepare_reset = !matches!(
        action_class,
        RefTxnActionClass::CommitLike
            | RefTxnActionClass::StashLike
            | RefTxnActionClass::CherryPickLike
    );

    if stage == "prepared" {
        let mut state = load_core_hook_state(repository)?;
        let before = state.clone();
        if saw_orig_head_update {
            state.pending_prepared_orig_head_ms = Some(now_ms());
            if action_class == RefTxnActionClass::PullRebaseLike {
                capture_pending_pull_autostash_state(repository, &mut state);
            }
        }

        if matches!(
            stash_transition_kind,
            Some(StashRefTransitionKind::AmbiguousReplace)
        ) && let Some(stash_count_before) = stash_entry_count(repository)
        {
            state.pending_stash_ref_update = Some(PendingStashRefUpdateState {
                created_at_ms: now_ms(),
                stash_count_before,
            });
        }

        let has_recent_orig_head = state
            .pending_prepared_orig_head_ms
            .map(|ts| now_ms().saturating_sub(ts) <= STATE_EVENT_MAX_AGE_MS)
            .unwrap_or(false);

        if has_recent_orig_head
            && let Some((_, target_head)) = moved_main_ref.as_ref()
            && !is_rebase_in_progress(repository)
            && may_prepare_reset
        {
            capture_pre_reset_state(repository, target_head);
            state.pending_prepared_orig_head_ms = None;
        }

        if let Some(ts) = state.pending_prepared_orig_head_ms
            && now_ms().saturating_sub(ts) > STATE_EVENT_MAX_AGE_MS
        {
            state.pending_prepared_orig_head_ms = None;
        }

        if let Some(pending) = state.pending_stash_ref_update.as_ref()
            && now_ms().saturating_sub(pending.created_at_ms) > STATE_EVENT_MAX_AGE_MS
        {
            state.pending_stash_ref_update = None;
        }

        // Drop stale pull-autostash snapshots that never got restored.
        if let Some(pending) = state.pending_pull_autostash.as_ref()
            && now_ms().saturating_sub(pending.created_at_ms) > PENDING_PULL_AUTOSTASH_MAX_AGE_MS
        {
            state.pending_pull_autostash = None;
        }
        save_core_hook_state_if_changed(repository, &before, &state)?;
        return Ok(());
    }

    let stash_count_before = if matches!(
        stash_transition_kind,
        Some(StashRefTransitionKind::AmbiguousReplace)
    ) {
        let mut state = load_core_hook_state(repository)?;
        let before = state.clone();
        let stash_count_before = state.pending_stash_ref_update.take().and_then(|pending| {
            if now_ms().saturating_sub(pending.created_at_ms) <= STATE_EVENT_MAX_AGE_MS {
                Some(pending.stash_count_before)
            } else {
                None
            }
        });
        save_core_hook_state_if_changed(repository, &before, &state)?;
        stash_count_before
    } else {
        None
    };

    let mut cache = HookInvocationCache::default();
    let stash_count_after = if matches!(
        stash_transition_kind,
        Some(StashRefTransitionKind::AmbiguousReplace)
    ) {
        cache.stash_count(repository)
    } else {
        None
    };
    let (created_stash_sha, deleted_stash_sha) = stash_ref_update
        .as_ref()
        .map(|(old, new)| {
            resolve_stash_ref_transition(
                old,
                new,
                stash_count_before,
                stash_count_after,
                reflog_action_value.as_deref(),
            )
        })
        .unwrap_or((None, None));

    let auto_merge_created = created_auto_merge_sha.is_some();

    for remote in remotes_to_sync {
        let _ = fetch_authorship_notes(repository, &remote);
    }

    if auto_merge_created {
        mark_pending_stash_apply(repository)?;
    }

    if let Some(stash_sha) = created_stash_sha {
        let _ = handle_stash_created(repository, &stash_sha);
    }

    if let Some(stash_sha) = deleted_stash_sha {
        if should_restore_deleted_stash(auto_merge_created, reflog_action_value.as_deref()) {
            let _ = restore_stash_attributions_from_sha(repository, &stash_sha);
            clear_pending_stash_apply(repository)?;
        } else {
            debug_log(&format!(
                "Skipping stash attribution restore for deleted stash {} (likely stash drop)",
                stash_sha
            ));
        }
    }

    if let Some(source_commit) = created_cherry_pick_head {
        let _ = set_pending_cherry_pick_state(repository, &source_commit);
    }

    if deleted_cherry_pick_head.is_some()
        && cache
            .reflog_subject(repository)
            .as_deref()
            .map(|s| s.contains("cherry-pick") && s.contains("abort"))
            .unwrap_or(false)
    {
        let _ = clear_pending_cherry_pick_state(repository);
    }

    // Track reset operations from reflog instead of command env.
    if let Some((old_head, new_head)) = moved_main_ref
        && !is_rebase_in_progress(repository)
        && may_prepare_reset
        && cache
            .reflog_subject(repository)
            .as_deref()
            .map(|s| s.starts_with("reset:"))
            .unwrap_or(false)
    {
        let mode = detect_reset_mode_from_worktree(repository);
        let _ = apply_reset_side_effects(repository, &old_head, &new_head, mode);
    }

    let is_pull_rebase_finish = action_class == RefTxnActionClass::PullRebaseLike
        && cache
            .reflog_subject(repository)
            .as_deref()
            .map(|s| s.starts_with("pull --rebase (finish):"))
            .unwrap_or(false);
    if is_pull_rebase_finish && let Some(start_event) = active_rebase_start_event(repository) {
        process_rebase_completion_from_start(repository, start_event);
    }

    if is_pull_rebase_finish
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StashRefTransitionKind {
    Created,
    Deleted,
    AmbiguousReplace,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StashRefTransition {
    Created {
        stash_sha: String,
    },
    Deleted {
        stash_sha: String,
    },
    AmbiguousReplace {
        old_stash_sha: String,
        new_stash_sha: String,
    },
    Unchanged,
}

fn parse_reference_transaction_line(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.split_whitespace();
    let old = parts.next()?;
    let new = parts.next()?;
    let reference = parts.next()?;
    Some((old, new, reference))
}

fn classify_stash_ref_transition_kind(old: &str, new: &str) -> StashRefTransitionKind {
    if old == new {
        return StashRefTransitionKind::Unchanged;
    }

    if is_zero_oid(old) && !is_zero_oid(new) {
        return StashRefTransitionKind::Created;
    }

    if !is_zero_oid(old) && is_zero_oid(new) {
        return StashRefTransitionKind::Deleted;
    }

    if !is_zero_oid(old) && !is_zero_oid(new) {
        return StashRefTransitionKind::AmbiguousReplace;
    }

    StashRefTransitionKind::Unchanged
}

fn classify_stash_ref_transition(old: &str, new: &str) -> StashRefTransition {
    match classify_stash_ref_transition_kind(old, new) {
        StashRefTransitionKind::Created => StashRefTransition::Created {
            stash_sha: new.to_string(),
        },
        StashRefTransitionKind::Deleted => StashRefTransition::Deleted {
            stash_sha: old.to_string(),
        },
        StashRefTransitionKind::AmbiguousReplace => StashRefTransition::AmbiguousReplace {
            old_stash_sha: old.to_string(),
            new_stash_sha: new.to_string(),
        },
        StashRefTransitionKind::Unchanged => StashRefTransition::Unchanged,
    }
}

fn resolve_stash_ref_transition(
    old: &str,
    new: &str,
    stash_count_before: Option<usize>,
    stash_count_after: Option<usize>,
    reflog_action: Option<&str>,
) -> (Option<String>, Option<String>) {
    match classify_stash_ref_transition(old, new) {
        StashRefTransition::Created { stash_sha } => (Some(stash_sha), None),
        StashRefTransition::Deleted { stash_sha } => (None, Some(stash_sha)),
        StashRefTransition::AmbiguousReplace {
            old_stash_sha,
            new_stash_sha,
        } => match (stash_count_before, stash_count_after) {
            (Some(before), Some(after)) if after > before => (Some(new_stash_sha), None),
            (Some(before), Some(after)) if after < before => (None, Some(old_stash_sha)),
            _ if reflog_action
                .map(|action| action.starts_with("stash push") || action == "stash")
                .unwrap_or(false) =>
            {
                (Some(new_stash_sha), None)
            }
            _ if reflog_action
                .map(|action| action.starts_with("stash pop") || action.starts_with("stash drop"))
                .unwrap_or(false) =>
            {
                (None, Some(old_stash_sha))
            }
            _ => (None, None),
        },
        StashRefTransition::Unchanged => (None, None),
    }
}

fn should_restore_deleted_stash(auto_merge_created: bool, reflog_action: Option<&str>) -> bool {
    if auto_merge_created {
        return true;
    }

    reflog_action
        .map(|action| action.starts_with("stash pop"))
        .unwrap_or(false)
}

fn stash_entry_count(repository: &Repository) -> Option<usize> {
    list_stash_shas(repository)
        .ok()
        .map(|entries| entries.len())
}

fn build_rebase_complete_event_from_start(
    start_event: &crate::git::rewrite_log::RebaseStartEvent,
    new_head: String,
    original_commits: Vec<String>,
    new_commits: Vec<String>,
) -> RewriteLogEvent {
    RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
        start_event.original_head.clone(),
        new_head,
        start_event.is_interactive,
        original_commits,
        new_commits,
    ))
}

fn rev_parse(repository: &Repository, revision: &str) -> Option<String> {
    let mut args = repository.global_args_for_exec();
    args.push("rev-parse".to_string());
    args.push(revision.to_string());

    let output = crate::git::repository::exec_git(&args).ok()?;
    if !output.status.success() {
        return None;
    }

    let resolved = String::from_utf8(output.stdout).ok()?;
    let resolved = resolved.trim();
    if resolved.is_empty() {
        None
    } else {
        Some(resolved.to_string())
    }
}

fn read_last_head_reflog_entry(repository: &Repository) -> Option<HeadReflogEntry> {
    let head_log_path = repository.path().join("logs").join("HEAD");
    let content = fs::read_to_string(head_log_path).ok()?;
    let line = content.lines().next_back()?.trim();
    if line.is_empty() {
        return None;
    }

    let (meta, subject) = line.split_once('\t')?;
    let mut parts = meta.split_whitespace();
    let old_sha = parts.next()?.to_string();
    let new_sha = parts.next()?.to_string();
    if old_sha.is_empty() || new_sha.is_empty() {
        return None;
    }

    Some(HeadReflogEntry {
        old_sha,
        new_sha,
        subject: subject.trim().to_string(),
    })
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

fn non_zero_oid(oid: &str) -> Option<String> {
    let oid = oid.trim();
    if oid.is_empty() || is_zero_oid(oid) {
        None
    } else {
        Some(oid.to_string())
    }
}

fn reflog_action() -> Option<String> {
    std::env::var("GIT_REFLOG_ACTION")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn classify_ref_transaction_action(action: Option<&str>) -> RefTxnActionClass {
    let Some(action) = action.map(str::trim).filter(|action| !action.is_empty()) else {
        return RefTxnActionClass::Unknown;
    };

    if action.starts_with("pull --rebase") {
        return RefTxnActionClass::PullRebaseLike;
    }
    if action.starts_with("reset") {
        return RefTxnActionClass::ResetLike;
    }
    if action.starts_with("rebase") {
        return RefTxnActionClass::RebaseLike;
    }
    if action.starts_with("stash") {
        return RefTxnActionClass::StashLike;
    }
    if action.starts_with("cherry-pick") {
        return RefTxnActionClass::CherryPickLike;
    }
    if action.starts_with("commit") {
        return RefTxnActionClass::CommitLike;
    }

    RefTxnActionClass::Unknown
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

fn pending_stash_apply_marker_path(repository: &Repository) -> PathBuf {
    repository
        .path()
        .join("ai")
        .join(PENDING_STASH_APPLY_MARKER_FILE)
}

fn ensure_pending_stash_apply_marker(repository: &Repository) -> Result<(), GitAiError> {
    let marker_path = pending_stash_apply_marker_path(repository);
    if marker_path.exists() {
        return Ok(());
    }
    if let Some(parent) = marker_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(marker_path, b"")?;
    Ok(())
}

fn remove_pending_stash_apply_marker(repository: &Repository) -> Result<(), GitAiError> {
    let marker_path = pending_stash_apply_marker_path(repository);
    if marker_path.exists() {
        fs::remove_file(marker_path)?;
    }
    Ok(())
}

fn mark_pending_stash_apply(repository: &Repository) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    let before = state.clone();
    if state.pending_stash_apply.is_none() {
        state.pending_stash_apply = Some(PendingStashApplyState {
            created_at_ms: now_ms(),
        });
    }
    save_core_hook_state_if_changed(repository, &before, &state)?;
    ensure_pending_stash_apply_marker(repository)
}

fn clear_pending_stash_apply(repository: &Repository) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    let before = state.clone();
    state.pending_stash_apply = None;
    save_core_hook_state_if_changed(repository, &before, &state)?;
    remove_pending_stash_apply_marker(repository)
}

fn maybe_restore_stash_apply_without_pop(repository: &Repository) -> Result<(), GitAiError> {
    let mut state = load_core_hook_state(repository)?;
    let Some(pending) = state.pending_stash_apply.clone() else {
        let _ = remove_pending_stash_apply_marker(repository);
        return Ok(());
    };

    if now_ms().saturating_sub(pending.created_at_ms) > STATE_EVENT_MAX_AGE_MS {
        state.pending_stash_apply = None;
        save_core_hook_state(repository, &state)?;
        remove_pending_stash_apply_marker(repository)?;
        return Ok(());
    }

    let Some(candidate) = find_best_matching_stash_with_note(repository)? else {
        return Ok(());
    };

    let _ = restore_stash_attributions_from_sha(repository, &candidate);
    state.pending_stash_apply = None;
    save_core_hook_state(repository, &state)?;
    remove_pending_stash_apply_marker(repository)
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
    let before = state.clone();
    state.pending_cherry_pick = None;
    save_core_hook_state_if_changed(repository, &before, &state)
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
        let event = build_rebase_complete_event_from_start(
            &start_event,
            new_head.clone(),
            original_commits,
            new_commits,
        );
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
            && entry.unstaged != crate::git::status::StatusCode::Untracked
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

fn has_non_empty_working_logs(repository: &Repository) -> bool {
    let working_logs_dir = repository.path().join("ai").join("working_logs");
    let Ok(entries) = fs::read_dir(working_logs_dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry_path.join("checkpoints.jsonl").is_file()
            && entry_path
                .join("checkpoints.jsonl")
                .metadata()
                .map(|meta| meta.len() > 0)
                .unwrap_or(false)
        {
            return true;
        }

        if entry_path.join(WORKING_LOG_INITIAL_FILE).is_file()
            && entry_path
                .join(WORKING_LOG_INITIAL_FILE)
                .metadata()
                .map(|meta| meta.len() > 0)
                .unwrap_or(false)
        {
            return true;
        }

        let blobs_dir = entry_path.join("blobs");
        if blobs_dir.is_dir()
            && fs::read_dir(blobs_dir)
                .map(|mut dir| dir.next().is_some())
                .unwrap_or(false)
        {
            return true;
        }
    }

    false
}

fn has_ai_notes_ref(repository: &Repository) -> bool {
    let git_dir = repository.path();
    if git_dir.join("refs").join("notes").join("ai").is_file() {
        return true;
    }

    let packed_refs = git_dir.join("packed-refs");
    let Ok(content) = fs::read_to_string(packed_refs) else {
        return false;
    };

    content
        .lines()
        .any(|line| line.trim_end().ends_with(" refs/notes/ai"))
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

fn first_parent_of_commit(repository: &Repository, commit_sha: &str) -> Option<String> {
    let revision = format!("{}^1", commit_sha);
    rev_parse(repository, &revision)
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

fn save_core_hook_state_if_changed(
    repository: &Repository,
    before: &CoreHookState,
    after: &CoreHookState,
) -> Result<(), GitAiError> {
    if before == after {
        return Ok(());
    }
    save_core_hook_state(repository, after)
}

fn core_hook_state_path(repository: &Repository) -> PathBuf {
    repository.path().join("ai").join(CORE_HOOK_STATE_FILE)
}

fn home_dir_from_env() -> Option<PathBuf> {
    for key in ["GIT_AI_HOME", "HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            return Some(PathBuf::from(value));
        }
    }

    #[cfg(windows)]
    {
        if let (Some(home_drive), Some(home_path)) =
            (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
            && !home_drive.is_empty()
            && !home_path.is_empty()
        {
            let mut combined = PathBuf::from(home_drive);
            combined.push(home_path);
            return Some(combined);
        }
    }

    None
}

/// Returns the managed global core-hooks directory.
pub fn managed_core_hooks_dir() -> Result<PathBuf, GitAiError> {
    let home = home_dir_from_env()
        .or_else(dirs::home_dir)
        .ok_or_else(|| GitAiError::Generic("Unable to determine home directory".to_string()))?;
    Ok(home.join(".git-ai").join("core-hooks"))
}

/// Writes git hook shims that dispatch to `git-ai hook-trampoline <hook-name>`.
pub fn write_core_hook_scripts(hooks_dir: &Path, git_ai_binary: &Path) -> Result<(), GitAiError> {
    fs::create_dir_all(hooks_dir)?;
    let binary = normalize_hook_binary_path(git_ai_binary);

    for hook in INSTALLED_HOOKS {
        let is_passthrough = PASSTHROUGH_ONLY_HOOKS.contains(hook);
        let script = if *hook == "reference-transaction" {
            format!(
                r#"#!/bin/sh
# git-ai-managed: mode=trampoline;type=ref-prefilter
if [ "${{{skip_env}:-}}" = "1" ]; then
  exit 0
fi
export {git_cmd_env}="${{{git_cmd_env}:-git}}"

script_dir="$0"
script_dir=$(printf '%s' "$script_dir" | tr '\\' '/')
case "$script_dir" in
  */*) script_dir="${{script_dir%/*}}" ;;
  *) script_dir="." ;;
esac
previous_hooks_file="$script_dir/{previous_hooks_file}"
previous_hooks_dir=""

if [ -s "$previous_hooks_file" ]; then
  IFS= read -r previous_hooks_dir < "$previous_hooks_file" || true
  case "$previous_hooks_dir" in
    "~") previous_hooks_dir="$HOME" ;;
    "~/"*) previous_hooks_dir="$HOME/${{previous_hooks_dir#\~/}}" ;;
  esac
fi
previous_hooks_dir=$(printf '%s' "$previous_hooks_dir" | tr '\\' '/')

if [ -n "$previous_hooks_dir" ]; then
  case "$script_dir" in */) script_dir="${{script_dir%/}}" ;; esac
  case "$previous_hooks_dir" in */) previous_hooks_dir="${{previous_hooks_dir%/}}" ;; esac
  if [ "$script_dir" = "$previous_hooks_dir" ]; then
    previous_hooks_dir=""
  fi
fi

repo_git_dir="${{GIT_DIR:-.git}}"
repo_git_dir=$(printf '%s' "$repo_git_dir" | tr '\\' '/')
if [ -n "$previous_hooks_dir" ]; then
  chain_hook="$previous_hooks_dir/{hook}"
else
  chain_hook="$repo_git_dir/hooks/{hook}"
fi
chain_kind="none"
if [ -x "$chain_hook" ]; then
  chain_kind="exec"
elif [ -f "$chain_hook" ]; then
  chain_kind="sh"
fi

dispatch=0
stage="$1"
action="${{GIT_REFLOG_ACTION:-}}"

is_zero_oid() {{
  case "$1" in
    ''|*[!0]*) return 1 ;;
    *) return 0 ;;
  esac
}}

# Fast path for default repos with no chained hooks: avoid stdin spooling and process startup.
if [ "$chain_kind" = "none" ]; then
  if [ "$stage" != "prepared" ] && [ "$stage" != "committed" ]; then
    exit 0
  fi
  case "$action" in
    commit*) exit 0 ;;
  esac

  IFS= read -r first_line || exit 0
  old_sha=""
  new_sha=""
  reference=""
  IFS=' ' read -r old_sha new_sha reference _rest <<EOF
$first_line
EOF

  first_line_relevant=0
  if [ -n "$old_sha" ] && [ -n "$new_sha" ] && [ -n "$reference" ] && [ "$old_sha" != "$new_sha" ]; then
    case "$reference" in
      ORIG_HEAD|HEAD|refs/heads/*|refs/stash|CHERRY_PICK_HEAD|refs/remotes/*)
        first_line_relevant=1
        ;;
      AUTO_MERGE)
        if is_zero_oid "$old_sha" && ! is_zero_oid "$new_sha"; then
          first_line_relevant=1
        fi
        ;;
    esac
  fi

  if [ $first_line_relevant -eq 1 ]; then
    rest_payload=$(cat)
    if [ -n "$rest_payload" ]; then
      stdin_payload="$first_line
$rest_payload"
      printf '%s\n' "$stdin_payload" | {skip_chain_env}=1 "{bin}" hook-trampoline "{hook}" "$@"
    else
      printf '%s\n' "$first_line" | {skip_chain_env}=1 "{bin}" hook-trampoline "{hook}" "$@"
    fi
    exit $?
  fi

  # Most callbacks are single-line and irrelevant; avoid extra parsing in that common case.
  IFS= read -r second_line || exit 0
  stdin_payload="$first_line
$second_line"
  rest_payload=$(cat)
  if [ -n "$rest_payload" ]; then
    stdin_payload="$stdin_payload
$rest_payload"
  fi

  zero_oid="0000000000000000000000000000000000000000"
  case "$stdin_payload" in
    *" ORIG_HEAD"*|*" HEAD"*|*" refs/heads/"*|*" refs/stash"*|*" CHERRY_PICK_HEAD"*|*" refs/remotes/"*)
      dispatch=1
      ;;
    *AUTO_MERGE*)
      case "$stdin_payload" in
        *"$zero_oid $zero_oid AUTO_MERGE"*)
          dispatch=0
          ;;
        *)
          dispatch=1
          ;;
      esac
      ;;
  esac

  if [ $dispatch -eq 1 ]; then
    printf '%s\n' "$stdin_payload" | {skip_chain_env}=1 "{bin}" hook-trampoline "{hook}" "$@"
    exit $?
  fi
  exit 0
fi

# Chained-hook path: preserve stdin for both dispatch and chained execution.
stdin_file=$(mktemp "${{TMPDIR:-/tmp}}/git-ai-ref-txn.XXXXXX") || exit 1
cleanup() {{
  rm -f "$stdin_file"
}}
trap cleanup EXIT
cat > "$stdin_file"

if [ "$stage" = "prepared" ] || [ "$stage" = "committed" ]; then
  case "$action" in
    commit*) dispatch=0 ;;
    *)
      while IFS=' ' read -r old_sha new_sha reference rest; do
        if [ "$old_sha" = "$new_sha" ]; then
          continue
        fi
        case "$reference" in
          ORIG_HEAD|HEAD|refs/heads/*|refs/stash|CHERRY_PICK_HEAD|refs/remotes/*)
            dispatch=1
            break
            ;;
          AUTO_MERGE)
            if is_zero_oid "$old_sha" && ! is_zero_oid "$new_sha"; then
              dispatch=1
              break
            fi
            ;;
        esac
      done < "$stdin_file"
      ;;
  esac
fi

if [ $dispatch -eq 1 ]; then
  {skip_chain_env}=1 "{bin}" hook-trampoline "{hook}" "$@" < "$stdin_file"
  dispatch_status=$?
  if [ $dispatch_status -ne 0 ]; then
    exit $dispatch_status
  fi
fi

if [ "$chain_kind" = "exec" ]; then
  "$chain_hook" "$@" < "$stdin_file"
  chain_status=$?
  if [ $chain_status -ne 0 ]; then
    exit $chain_status
  fi
elif [ "$chain_kind" = "sh" ]; then
  sh "$chain_hook" "$@" < "$stdin_file"
  chain_status=$?
  if [ $chain_status -ne 0 ]; then
    exit $chain_status
  fi
fi

exit 0
"#,
                skip_env = GIT_AI_SKIP_CORE_HOOKS_ENV,
                git_cmd_env = GIT_AI_GIT_CMD_ENV,
                skip_chain_env = GIT_AI_TRAMPOLINE_SKIP_CHAIN_ENV,
                previous_hooks_file = PREVIOUS_HOOKS_PATH_FILE,
                bin = binary,
                hook = hook,
            )
        } else if *hook == "pre-commit" {
            format!(
                r#"#!/bin/sh
# git-ai-managed: mode=trampoline;type=pre-commit-prefilter
if [ "${{{skip_env}:-}}" = "1" ]; then
  exit 0
fi
export {git_cmd_env}="${{{git_cmd_env}:-git}}"

script_dir="$0"
script_dir=$(printf '%s' "$script_dir" | tr '\\' '/')
case "$script_dir" in
  */*) script_dir="${{script_dir%/*}}" ;;
  *) script_dir="." ;;
esac
previous_hooks_file="$script_dir/{previous_hooks_file}"
previous_hooks_dir=""

if [ -s "$previous_hooks_file" ]; then
  IFS= read -r previous_hooks_dir < "$previous_hooks_file" || true
  case "$previous_hooks_dir" in
    "~") previous_hooks_dir="$HOME" ;;
    "~/"*) previous_hooks_dir="$HOME/${{previous_hooks_dir#\~/}}" ;;
  esac
fi
previous_hooks_dir=$(printf '%s' "$previous_hooks_dir" | tr '\\' '/')

if [ -n "$previous_hooks_dir" ]; then
  case "$script_dir" in */) script_dir="${{script_dir%/}}" ;; esac
  case "$previous_hooks_dir" in */) previous_hooks_dir="${{previous_hooks_dir%/}}" ;; esac
  if [ "$script_dir" = "$previous_hooks_dir" ]; then
    previous_hooks_dir=""
  fi
fi

repo_git_dir="${{GIT_DIR:-.git}}"
repo_git_dir=$(printf '%s' "$repo_git_dir" | tr '\\' '/')
working_logs_dir="$repo_git_dir/ai/working_logs"
dispatch=0
if [ -d "$working_logs_dir" ]; then
  for entry in "$working_logs_dir"/*; do
    if [ -s "$entry/checkpoints.jsonl" ]; then
      dispatch=1
      break
    fi
    if [ -s "$entry/{initial_file}" ]; then
      dispatch=1
      break
    fi
    if [ -d "$entry/blobs" ]; then
      for blob_entry in "$entry/blobs"/*; do
        if [ -e "$blob_entry" ]; then
          dispatch=1
          break 2
        fi
      done
    fi
  done
fi

if [ $dispatch -eq 1 ]; then
  {skip_chain_env}=1 "{bin}" hook-trampoline "{hook}" "$@"
  dispatch_status=$?
  if [ $dispatch_status -ne 0 ]; then
    exit $dispatch_status
  fi
fi

if [ -n "$previous_hooks_dir" ]; then
  chain_hook="$previous_hooks_dir/{hook}"
else
  chain_hook="$repo_git_dir/hooks/{hook}"
fi
if [ -x "$chain_hook" ]; then
  "$chain_hook" "$@"
  exit $?
fi
if [ -f "$chain_hook" ]; then
  sh "$chain_hook" "$@"
  exit $?
fi
exit 0
"#,
                skip_env = GIT_AI_SKIP_CORE_HOOKS_ENV,
                git_cmd_env = GIT_AI_GIT_CMD_ENV,
                skip_chain_env = GIT_AI_TRAMPOLINE_SKIP_CHAIN_ENV,
                previous_hooks_file = PREVIOUS_HOOKS_PATH_FILE,
                initial_file = WORKING_LOG_INITIAL_FILE,
                bin = binary,
                hook = hook,
            )
        } else if *hook == "post-commit" {
            format!(
                r#"#!/bin/sh
# git-ai-managed: mode=trampoline;type=post-commit-prefilter
if [ "${{{skip_env}:-}}" = "1" ]; then
  exit 0
fi
export {git_cmd_env}="${{{git_cmd_env}:-git}}"

script_dir="$0"
script_dir=$(printf '%s' "$script_dir" | tr '\\' '/')
case "$script_dir" in
  */*) script_dir="${{script_dir%/*}}" ;;
  *) script_dir="." ;;
esac
previous_hooks_file="$script_dir/{previous_hooks_file}"
previous_hooks_dir=""

if [ -s "$previous_hooks_file" ]; then
  IFS= read -r previous_hooks_dir < "$previous_hooks_file" || true
  case "$previous_hooks_dir" in
    "~") previous_hooks_dir="$HOME" ;;
    "~/"*) previous_hooks_dir="$HOME/${{previous_hooks_dir#\~/}}" ;;
  esac
fi
previous_hooks_dir=$(printf '%s' "$previous_hooks_dir" | tr '\\' '/')

if [ -n "$previous_hooks_dir" ]; then
  case "$script_dir" in */) script_dir="${{script_dir%/}}" ;; esac
  case "$previous_hooks_dir" in */) previous_hooks_dir="${{previous_hooks_dir%/}}" ;; esac
  if [ "$script_dir" = "$previous_hooks_dir" ]; then
    previous_hooks_dir=""
  fi
fi

repo_git_dir="${{GIT_DIR:-.git}}"
repo_git_dir=$(printf '%s' "$repo_git_dir" | tr '\\' '/')
dispatch=0
state_file="$repo_git_dir/ai/core_hook_state.json"
working_logs_dir="$repo_git_dir/ai/working_logs"

if [ -f "$repo_git_dir/CHERRY_PICK_HEAD" ]; then
  dispatch=1
fi

if [ $dispatch -eq 0 ] && [ -f "$state_file" ]; then
  IFS= read -r state_line < "$state_file" || true
  case "$state_line" in
    *'"pending_cherry_pick":{{'*)
      dispatch=1
      ;;
  esac
fi

if [ $dispatch -eq 0 ] && [ -d "$working_logs_dir" ]; then
  for entry in "$working_logs_dir"/*; do
    if [ -s "$entry/checkpoints.jsonl" ]; then
      dispatch=1
      break
    fi
    if [ -s "$entry/{initial_file}" ]; then
      dispatch=1
      break
    fi
    if [ -d "$entry/blobs" ]; then
      for blob_entry in "$entry/blobs"/*; do
        if [ -e "$blob_entry" ]; then
          dispatch=1
          break 2
        fi
      done
    fi
  done
fi

if [ $dispatch -eq 0 ]; then
  if [ -f "$repo_git_dir/refs/notes/ai" ]; then
    dispatch=1
  elif [ -f "$repo_git_dir/packed-refs" ]; then
    while IFS= read -r ref_line; do
      case "$ref_line" in
        *" refs/notes/ai")
          dispatch=1
          break
          ;;
      esac
    done < "$repo_git_dir/packed-refs"
  fi
fi

if [ $dispatch -eq 1 ]; then
  {skip_chain_env}=1 "{bin}" hook-trampoline "{hook}" "$@"
  dispatch_status=$?
  if [ $dispatch_status -ne 0 ]; then
    exit $dispatch_status
  fi
fi

if [ -n "$previous_hooks_dir" ]; then
  chain_hook="$previous_hooks_dir/{hook}"
else
  chain_hook="$repo_git_dir/hooks/{hook}"
fi
if [ -x "$chain_hook" ]; then
  "$chain_hook" "$@"
  exit $?
fi
if [ -f "$chain_hook" ]; then
  sh "$chain_hook" "$@"
  exit $?
fi
exit 0
"#,
                skip_env = GIT_AI_SKIP_CORE_HOOKS_ENV,
                git_cmd_env = GIT_AI_GIT_CMD_ENV,
                skip_chain_env = GIT_AI_TRAMPOLINE_SKIP_CHAIN_ENV,
                previous_hooks_file = PREVIOUS_HOOKS_PATH_FILE,
                initial_file = WORKING_LOG_INITIAL_FILE,
                bin = binary,
                hook = hook,
            )
        } else if *hook == "post-index-change" {
            format!(
                r#"#!/bin/sh
# git-ai-managed: mode=trampoline;type=post-index-prefilter
if [ "${{{skip_env}:-}}" = "1" ]; then
  exit 0
fi
export {git_cmd_env}="${{{git_cmd_env}:-git}}"

script_dir="$0"
script_dir=$(printf '%s' "$script_dir" | tr '\\' '/')
case "$script_dir" in
  */*) script_dir="${{script_dir%/*}}" ;;
  *) script_dir="." ;;
esac
previous_hooks_file="$script_dir/{previous_hooks_file}"
previous_hooks_dir=""

if [ -s "$previous_hooks_file" ]; then
  IFS= read -r previous_hooks_dir < "$previous_hooks_file" || true
  case "$previous_hooks_dir" in
    "~") previous_hooks_dir="$HOME" ;;
    "~/"*) previous_hooks_dir="$HOME/${{previous_hooks_dir#\~/}}" ;;
  esac
fi
previous_hooks_dir=$(printf '%s' "$previous_hooks_dir" | tr '\\' '/')

if [ -n "$previous_hooks_dir" ]; then
  case "$script_dir" in */) script_dir="${{script_dir%/}}" ;; esac
  case "$previous_hooks_dir" in */) previous_hooks_dir="${{previous_hooks_dir%/}}" ;; esac
  if [ "$script_dir" = "$previous_hooks_dir" ]; then
    previous_hooks_dir=""
  fi
fi

repo_git_dir="${{GIT_DIR:-.git}}"
repo_git_dir=$(printf '%s' "$repo_git_dir" | tr '\\' '/')
marker_file="$repo_git_dir/ai/{pending_stash_marker_file}"
dispatch=0
if [ -f "$marker_file" ]; then
  dispatch=1
fi

if [ $dispatch -eq 1 ]; then
  {skip_chain_env}=1 "{bin}" hook-trampoline "{hook}" "$@"
  dispatch_status=$?
  if [ $dispatch_status -ne 0 ]; then
    exit $dispatch_status
  fi
fi

if [ -n "$previous_hooks_dir" ]; then
  chain_hook="$previous_hooks_dir/{hook}"
else
  chain_hook="$repo_git_dir/hooks/{hook}"
fi

if [ -x "$chain_hook" ]; then
  "$chain_hook" "$@"
  exit $?
fi
if [ -f "$chain_hook" ]; then
  sh "$chain_hook" "$@"
  exit $?
fi

exit 0
"#,
                skip_env = GIT_AI_SKIP_CORE_HOOKS_ENV,
                git_cmd_env = GIT_AI_GIT_CMD_ENV,
                skip_chain_env = GIT_AI_TRAMPOLINE_SKIP_CHAIN_ENV,
                previous_hooks_file = PREVIOUS_HOOKS_PATH_FILE,
                pending_stash_marker_file = PENDING_STASH_APPLY_MARKER_FILE,
                bin = binary,
                hook = hook,
            )
        } else if *hook == "commit-msg" || *hook == "prepare-commit-msg" {
            format!(
                r#"#!/bin/sh
# git-ai-managed: mode=passthrough-shell
if [ "${{{skip_env}:-}}" = "1" ]; then
  exit 0
fi

script_dir="$0"
script_dir=$(printf '%s' "$script_dir" | tr '\\' '/')
case "$script_dir" in
  */*) script_dir="${{script_dir%/*}}" ;;
  *) script_dir="." ;;
esac
previous_hooks_dir=""
previous_hooks_file="$script_dir/{previous_hooks_file}"
if [ -s "$previous_hooks_file" ]; then
  IFS= read -r previous_hooks_dir < "$previous_hooks_file" || true
  case "$previous_hooks_dir" in
    "~") previous_hooks_dir="$HOME" ;;
    "~/"*) previous_hooks_dir="$HOME/${{previous_hooks_dir#\~/}}" ;;
  esac
fi
previous_hooks_dir=$(printf '%s' "$previous_hooks_dir" | tr '\\' '/')

if [ -n "$previous_hooks_dir" ]; then
  case "$script_dir" in */) script_dir="${{script_dir%/}}" ;; esac
  case "$previous_hooks_dir" in */) previous_hooks_dir="${{previous_hooks_dir%/}}" ;; esac
  if [ "$script_dir" = "$previous_hooks_dir" ]; then
    previous_hooks_dir=""
  fi
fi

repo_git_dir="${{GIT_DIR:-.git}}"
repo_git_dir=$(printf '%s' "$repo_git_dir" | tr '\\' '/')
if [ -n "$previous_hooks_dir" ]; then
  chain_hook="$previous_hooks_dir/{hook}"
else
  chain_hook="$repo_git_dir/hooks/{hook}"
fi

if [ -x "$chain_hook" ]; then
  "$chain_hook" "$@"
  exit $?
fi
if [ -f "$chain_hook" ]; then
  sh "$chain_hook" "$@"
  exit $?
fi
exit 0
"#,
                skip_env = GIT_AI_SKIP_CORE_HOOKS_ENV,
                previous_hooks_file = PREVIOUS_HOOKS_PATH_FILE,
                hook = hook,
            )
        } else if is_passthrough {
            format!(
                r#"#!/bin/sh
# git-ai-managed: mode=passthrough-shell
if [ "${{{skip_env}:-}}" = "1" ]; then
  exit 0
fi

script_dir="$0"
script_dir=$(printf '%s' "$script_dir" | tr '\\' '/')
case "$script_dir" in
  */*) script_dir="${{script_dir%/*}}" ;;
  *) script_dir="." ;;
esac
previous_hooks_file="$script_dir/{previous_hooks_file}"
previous_hooks_dir=""

if [ -s "$previous_hooks_file" ]; then
  IFS= read -r previous_hooks_dir < "$previous_hooks_file" || true
  case "$previous_hooks_dir" in
    "~") previous_hooks_dir="$HOME" ;;
    "~/"*) previous_hooks_dir="$HOME/${{previous_hooks_dir#\~/}}" ;;
  esac
fi
previous_hooks_dir=$(printf '%s' "$previous_hooks_dir" | tr '\\' '/')

if [ -n "$previous_hooks_dir" ]; then
  case "$script_dir" in */) script_dir="${{script_dir%/}}" ;; esac
  case "$previous_hooks_dir" in */) previous_hooks_dir="${{previous_hooks_dir%/}}" ;; esac
  if [ "$script_dir" = "$previous_hooks_dir" ]; then
    previous_hooks_dir=""
  fi
fi

repo_git_dir="${{GIT_DIR:-.git}}"
repo_git_dir=$(printf '%s' "$repo_git_dir" | tr '\\' '/')
if [ -n "$previous_hooks_dir" ]; then
  chain_hook="$previous_hooks_dir/{hook}"
else
  chain_hook="$repo_git_dir/hooks/{hook}"
fi
if [ -x "$chain_hook" ]; then
  "$chain_hook" "$@"
  exit $?
fi
if [ -f "$chain_hook" ]; then
  sh "$chain_hook" "$@"
  exit $?
fi

exit 0
"#,
                skip_env = GIT_AI_SKIP_CORE_HOOKS_ENV,
                previous_hooks_file = PREVIOUS_HOOKS_PATH_FILE,
                hook = hook,
            )
        } else {
            format!(
                r#"#!/bin/sh
# git-ai-managed: mode=trampoline;type=dispatch
if [ "${{{skip_env}:-}}" = "1" ]; then
  exit 0
fi
export {git_cmd_env}="${{{git_cmd_env}:-git}}"
exec "{bin}" hook-trampoline "{hook}" "$@"
"#,
                skip_env = GIT_AI_SKIP_CORE_HOOKS_ENV,
                git_cmd_env = GIT_AI_GIT_CMD_ENV,
                bin = binary,
                hook = hook,
            )
        };
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

pub(crate) fn normalize_hook_binary_path(git_ai_binary: &Path) -> String {
    git_ai_binary
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::{
        RefTxnActionClass, WORKING_LOG_INITIAL_FILE, active_rebase_start_event,
        build_rebase_complete_event_from_start, classify_ref_transaction_action,
        find_repository_for_hook, has_non_empty_working_logs, is_zero_oid,
        resolve_stash_ref_transition, should_restore_deleted_stash,
    };
    use crate::git::rewrite_log::{
        RebaseAbortEvent, RebaseCompleteEvent, RebaseStartEvent, RewriteLogEvent,
    };
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

    #[test]
    fn classify_ref_transaction_action_handles_known_actions() {
        assert_eq!(
            classify_ref_transaction_action(Some("commit (amend): msg")),
            RefTxnActionClass::CommitLike
        );
        assert_eq!(
            classify_ref_transaction_action(Some("reset: moving to HEAD~1")),
            RefTxnActionClass::ResetLike
        );
        assert_eq!(
            classify_ref_transaction_action(Some(
                "pull --rebase (finish): returning to refs/heads/main"
            )),
            RefTxnActionClass::PullRebaseLike
        );
        assert_eq!(
            classify_ref_transaction_action(Some("stash pop")),
            RefTxnActionClass::StashLike
        );
    }

    #[test]
    fn stash_ref_transition_nonzero_to_nonzero_with_depth_growth_is_creation() {
        let old_sha = "1111111111111111111111111111111111111111";
        let new_sha = "2222222222222222222222222222222222222222";

        let (created, deleted) =
            resolve_stash_ref_transition(old_sha, new_sha, Some(1), Some(2), None);

        assert_eq!(created.as_deref(), Some(new_sha));
        assert!(deleted.is_none());
    }

    #[test]
    fn stash_ref_transition_nonzero_to_nonzero_with_depth_shrink_is_deletion() {
        let old_sha = "1111111111111111111111111111111111111111";
        let new_sha = "2222222222222222222222222222222222222222";

        let (created, deleted) =
            resolve_stash_ref_transition(old_sha, new_sha, Some(2), Some(1), None);

        assert!(created.is_none());
        assert_eq!(deleted.as_deref(), Some(old_sha));
    }

    #[test]
    fn stash_ref_transition_nonzero_to_nonzero_without_depth_signal_is_ignored() {
        let old_sha = "1111111111111111111111111111111111111111";
        let new_sha = "2222222222222222222222222222222222222222";

        let (created, deleted) = resolve_stash_ref_transition(old_sha, new_sha, None, None, None);

        assert!(created.is_none());
        assert!(deleted.is_none());
    }

    #[test]
    fn restore_deleted_stash_requires_pop_signal() {
        assert!(should_restore_deleted_stash(true, None));
        assert!(should_restore_deleted_stash(false, Some("stash pop")));
        assert!(!should_restore_deleted_stash(false, Some("stash drop")));
        assert!(!should_restore_deleted_stash(false, None));
    }

    #[test]
    fn stash_ref_transition_nonzero_to_nonzero_uses_reflog_fallback_when_depth_missing() {
        let old_sha = "1111111111111111111111111111111111111111";
        let new_sha = "2222222222222222222222222222222222222222";

        let (created, deleted) =
            resolve_stash_ref_transition(old_sha, new_sha, None, None, Some("stash push"));
        assert_eq!(created.as_deref(), Some(new_sha));
        assert!(deleted.is_none());

        let (created, deleted) =
            resolve_stash_ref_transition(old_sha, new_sha, None, None, Some("stash pop"));
        assert!(created.is_none());
        assert_eq!(deleted.as_deref(), Some(old_sha));
    }

    #[test]
    fn is_zero_oid_rejects_empty_strings() {
        assert!(!is_zero_oid(""));
        assert!(is_zero_oid("0000000000000000000000000000000000000000"));
    }

    #[test]
    fn build_rebase_complete_event_from_start_uses_interactive_flag() {
        let start_event = RebaseStartEvent::new_with_onto(
            "old-head".to_string(),
            true,
            Some("onto-head".to_string()),
        );
        let event = build_rebase_complete_event_from_start(
            &start_event,
            "new-head".to_string(),
            vec!["old-commit".to_string()],
            vec!["new-commit".to_string()],
        );

        match event {
            RewriteLogEvent::RebaseComplete { rebase_complete } => {
                assert!(rebase_complete.is_interactive);
                assert_eq!(rebase_complete.original_head, "old-head");
                assert_eq!(rebase_complete.new_head, "new-head");
            }
            _ => panic!("expected RebaseComplete event"),
        }
    }

    #[test]
    fn active_rebase_start_event_returns_latest_unfinished_start() {
        let repo = TmpRepo::new().expect("tmp repo");
        let storage = &repo.gitai_repo().storage;

        let completed_start = RebaseStartEvent::new_with_onto(
            "1111111111111111111111111111111111111111".to_string(),
            false,
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
        );
        storage
            .append_rewrite_event(RewriteLogEvent::rebase_start(completed_start))
            .expect("append completed start");
        storage
            .append_rewrite_event(RewriteLogEvent::rebase_complete(RebaseCompleteEvent::new(
                "1111111111111111111111111111111111111111".to_string(),
                "3333333333333333333333333333333333333333".to_string(),
                false,
                vec![],
                vec![],
            )))
            .expect("append rebase complete");

        let active_start = RebaseStartEvent::new_with_onto(
            "4444444444444444444444444444444444444444".to_string(),
            true,
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
        );
        storage
            .append_rewrite_event(RewriteLogEvent::rebase_start(active_start.clone()))
            .expect("append active start");

        let active = active_rebase_start_event(repo.gitai_repo()).expect("active rebase start");
        assert_eq!(active, active_start);
    }

    #[test]
    fn active_rebase_start_event_returns_none_when_latest_rebase_is_closed() {
        let repo = TmpRepo::new().expect("tmp repo");
        let storage = &repo.gitai_repo().storage;

        storage
            .append_rewrite_event(RewriteLogEvent::rebase_start(RebaseStartEvent::new(
                "1111111111111111111111111111111111111111".to_string(),
                false,
            )))
            .expect("append rebase start");
        storage
            .append_rewrite_event(RewriteLogEvent::rebase_abort(RebaseAbortEvent::new(
                "1111111111111111111111111111111111111111".to_string(),
            )))
            .expect("append rebase abort");

        assert!(
            active_rebase_start_event(repo.gitai_repo()).is_none(),
            "latest closed rebase should not be treated as active"
        );
    }

    #[test]
    fn has_non_empty_working_logs_detects_initial_only_entries() {
        let repo = TmpRepo::new().expect("tmp repo");
        let repository = repo.gitai_repo();
        let working_log = repository.storage.working_log_for_base_commit("base-sha");
        std::fs::write(
            working_log.dir.join(WORKING_LOG_INITIAL_FILE),
            "{\"files\":{}}",
        )
        .expect("write INITIAL");

        assert!(has_non_empty_working_logs(repository));
    }
}
