use crate::authorship::attribution_recovery::{
    AttributionRecoveryContext, FileTimestampsByPath, UnknownLinesByFile,
};
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::ignore::{
    build_ignore_matcher, effective_ignore_patterns, should_ignore_file_with_matcher,
};
use crate::authorship::rewrite::DiffTreeResult;
use crate::authorship::stats::{stats_for_commit_stats_from_hunks, write_stats_to_terminal};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::{Checkpoint, CheckpointKind, WorkingLogEntry};
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::notes_api::write_note as notes_add;
use crate::git::repository::{Repository, batch_read_paths_at_treeishes};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;

/// Skip expensive post-commit stats when this threshold is exceeded.
/// High hunk density is the strongest predictor of slow diff_ai_accepted_stats.
#[doc(hidden)]
pub const STATS_SKIP_MAX_HUNKS: usize = 1000;
/// Skip expensive stats for very large net additions even if hunks are moderate.
#[doc(hidden)]
pub const STATS_SKIP_MAX_ADDED_LINES: usize = 6000;
/// Skip expensive stats for extremely wide commits touching many added-line files.
#[doc(hidden)]
pub const STATS_SKIP_MAX_FILES_WITH_ADDITIONS: usize = 200;
/// Skip expensive stats for commits that delete a large number of lines.
/// Deletion-heavy commits (e.g. removing many files) trigger the same expensive
/// diff-parsing path as large addition commits, but the added-lines estimate is
/// near zero, so the cost was previously invisible to the estimator.
#[doc(hidden)]
pub const STATS_SKIP_MAX_DELETED_LINES: usize = 6000;

#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct StatsCostEstimate {
    pub files_with_additions: usize,
    pub added_lines: usize,
    pub hunk_ranges: usize,
    pub deleted_lines: usize,
}

fn checkpoint_entry_requires_post_processing(
    checkpoint: &Checkpoint,
    entry: &WorkingLogEntry,
) -> bool {
    if checkpoint.kind != CheckpointKind::Human {
        return true;
    }

    entry
        .line_attributions
        .iter()
        .any(|attr| attr.author_id != CheckpointKind::Human.to_str() || attr.overrode.is_some())
        || entry
            .attributions
            .iter()
            .any(|attr| attr.author_id != CheckpointKind::Human.to_str())
}

pub fn post_commit(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
) -> Result<(String, AuthorshipLog), GitAiError> {
    post_commit_from_working_log(repo, base_commit, commit_sha, human_author, supress_output)
}

pub fn post_commit_from_working_log(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
) -> Result<(String, AuthorshipLog), GitAiError> {
    post_commit_from_working_log_with_transform(
        repo,
        base_commit,
        commit_sha,
        human_author,
        supress_output,
        Ok,
    )
}

pub(crate) fn post_commit_from_working_log_with_recovery_timestamps(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
    recovery_file_timestamps: Option<&FileTimestampsByPath>,
    before_external_recovery: Option<&dyn Fn(&UnknownLinesByFile)>,
) -> Result<(String, AuthorshipLog), GitAiError> {
    post_commit_from_working_log_with_transform_context(
        repo,
        base_commit,
        commit_sha,
        human_author,
        PostCommitOptions {
            supress_output,
            compute_stats: true,
            recover_attribution: true,
        },
        PostCommitContext {
            precomputed_parent_diff: None,
            recovery_file_timestamps,
            before_external_recovery,
        },
        Ok,
    )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PostCommitOptions {
    pub supress_output: bool,
    pub compute_stats: bool,
    pub recover_attribution: bool,
}

#[derive(Clone, Copy, Default)]
struct PostCommitContext<'a> {
    precomputed_parent_diff: Option<&'a DiffTreeResult>,
    recovery_file_timestamps: Option<&'a FileTimestampsByPath>,
    before_external_recovery: Option<&'a dyn Fn(&UnknownLinesByFile)>,
}

pub fn post_commit_from_working_log_with_transform<F>(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
    transform: F,
) -> Result<(String, AuthorshipLog), GitAiError>
where
    F: FnOnce(AuthorshipLog) -> Result<AuthorshipLog, GitAiError>,
{
    post_commit_from_working_log_with_transform_and_options(
        repo,
        base_commit,
        commit_sha,
        human_author,
        PostCommitOptions {
            supress_output,
            compute_stats: true,
            recover_attribution: true,
        },
        transform,
    )
}

pub(crate) fn post_commit_from_working_log_with_transform_and_options<F>(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    options: PostCommitOptions,
    transform: F,
) -> Result<(String, AuthorshipLog), GitAiError>
where
    F: FnOnce(AuthorshipLog) -> Result<AuthorshipLog, GitAiError>,
{
    post_commit_from_working_log_with_transform_options_and_diff(
        repo,
        base_commit,
        commit_sha,
        human_author,
        options,
        None,
        transform,
    )
}

/// As [`post_commit_from_working_log_with_transform_and_options`], but accepts a
/// pre-computed parent→commit `DiffTreeResult`. A batched caller (the rebase
/// conflict-resolution driver) computes every qualifying commit's parent→commit
/// diff in ONE `diff-tree` and threads each result here, so this function makes
/// no per-commit `git diff` / `git diff-tree` spawns. With `None` the behavior
/// is identical to the unbatched single-commit path.
pub(crate) fn post_commit_from_working_log_with_transform_options_and_diff<F>(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    options: PostCommitOptions,
    precomputed_parent_diff: Option<&DiffTreeResult>,
    transform: F,
) -> Result<(String, AuthorshipLog), GitAiError>
where
    F: FnOnce(AuthorshipLog) -> Result<AuthorshipLog, GitAiError>,
{
    post_commit_from_working_log_with_transform_context(
        repo,
        base_commit,
        commit_sha,
        human_author,
        options,
        PostCommitContext {
            precomputed_parent_diff,
            recovery_file_timestamps: None,
            before_external_recovery: None,
        },
        transform,
    )
}

fn post_commit_from_working_log_with_transform_context<F>(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    options: PostCommitOptions,
    context: PostCommitContext<'_>,
    transform: F,
) -> Result<(String, AuthorshipLog), GitAiError>
where
    F: FnOnce(AuthorshipLog) -> Result<AuthorshipLog, GitAiError>,
{
    // Use base_commit parameter if provided, otherwise use "initial" for empty repos
    // This matches the convention in checkpoint.rs
    let parent_sha = base_commit.unwrap_or_else(|| "initial".to_string());

    // Initialize the new storage system
    let repo_storage = &repo.storage;
    let working_log = repo_storage.working_log_for_base_commit(&parent_sha)?;

    let parent_working_log = working_log.read_all_checkpoints()?;

    let observed_snapshot = working_log.observed_file_snapshot()?;
    let working_va = VirtualAttributions::from_persisted_working_log(
        repo.clone(),
        parent_sha.clone(),
        Some(human_author.clone()),
    )?;

    // Build pathspecs from AI-relevant checkpoint entries only.
    // Human-only entries with no AI attribution do not affect authorship output and should not
    // trigger expensive post-commit diff work across large commits.
    let mut pathspecs: HashSet<String> = HashSet::new();
    for checkpoint in &parent_working_log {
        for entry in &checkpoint.entries {
            if checkpoint_entry_requires_post_processing(checkpoint, entry) {
                pathspecs.insert(entry.file.clone());
            }
        }
    }

    // Also include files from INITIAL attributions (uncommitted files from previous commits)
    // These files may not have checkpoints but still need their attribution preserved
    // when they are finally committed. See issue #356.
    let initial_attributions_for_pathspecs = working_log.read_initial_attributions();
    for file_path in initial_attributions_for_pathspecs.files.keys() {
        pathspecs.insert(file_path.clone());
    }

    let (mut authorship_log, initial_attributions, initial_file_contents) = working_va
        .to_authorship_log_and_initial_working_log_with_precomputed_diff(
            repo,
            &parent_sha,
            &commit_sha,
            Some(&pathspecs),
            Some(&observed_snapshot),
            context.precomputed_parent_diff,
        )?;

    authorship_log.metadata.base_commit_sha = commit_sha.clone();

    // No-hooks background agents (Devin, Codex Cloud, etc.) may not fire checkpoints
    // for all edits. Attribute any committed lines that have no existing attestation
    // ("holes") to the detected agent, preserving explicit attributions.
    if !matches!(
        crate::authorship::background_agent::detect(),
        crate::authorship::background_agent::BackgroundAgent::None
            | crate::authorship::background_agent::BackgroundAgent::WithHooks { .. }
    ) {
        // Prefer the batched parent→commit diff when supplied (no extra spawn);
        // otherwise fall back to a per-commit `git diff`.
        let committed_hunks: Option<
            HashMap<String, Vec<crate::authorship::authorship_log::LineRange>>,
        > = if let Some(diff) = context.precomputed_parent_diff {
            Some(
                crate::authorship::virtual_attribution::committed_hunks_from_diff_result(
                    diff, None,
                ),
            )
        } else {
            let diff_base = if parent_sha == "initial" {
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
            } else {
                &parent_sha
            };
            repo.diff_added_lines(diff_base, &commit_sha, None)
                .ok()
                .map(|added_lines| {
                    added_lines
                        .into_iter()
                        .filter(|(_, lines)| !lines.is_empty())
                        .map(|(path, lines)| {
                            (
                                path,
                                crate::authorship::authorship_log::LineRange::compress_lines(
                                    &lines,
                                ),
                            )
                        })
                        .collect()
                })
        };
        if let Some(committed_hunks) = committed_hunks {
            crate::authorship::background_agent::fill_unattributed_lines(
                &mut authorship_log,
                &committed_hunks,
                &human_author,
            );
        }
    }

    authorship_log = transform(authorship_log)?;
    authorship_log.metadata.base_commit_sha = commit_sha.clone();

    if options.recover_attribution {
        let recovery_hunks = recovery_committed_hunks(
            repo,
            &parent_sha,
            &commit_sha,
            context.precomputed_parent_diff,
        )?;
        crate::authorship::attribution_recovery::recover_attribution(
            repo,
            &parent_sha,
            &commit_sha,
            &human_author,
            &mut authorship_log,
            &recovery_hunks,
            AttributionRecoveryContext {
                file_timestamps: context.recovery_file_timestamps,
                before_external_recovery: context.before_external_recovery,
            },
        )?;
        authorship_log.metadata.base_commit_sha = commit_sha.clone();
    }

    // Long-lived daemon processes should read a fresh config snapshot.
    // Always use Config::fresh() to support runtime config updates
    // (especially important for daemon mode, but also good for consistency)
    let config = Config::fresh();
    let custom_attrs = config.custom_attributes().clone();

    // Inject custom attributes into all PromptRecords and SessionRecords.
    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
        for sr in authorship_log.metadata.sessions.values_mut() {
            sr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    let authorship_note_str = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    notes_add(repo, &commit_sha, &authorship_note_str)?;

    // Compute stats once (needed for both metrics and terminal output), unless preflight
    // estimate predicts this would be too expensive for the commit hook path.
    let mut stats: Option<crate::authorship::stats::CommitStats> = None;
    let mut skip_reason = None;

    if options.compute_stats {
        let is_merge_commit = repo
            .find_commit(commit_sha.clone())
            .map(|commit| commit.parent_count().unwrap_or(0) > 1)
            .unwrap_or(false);
        let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
        skip_reason = if is_merge_commit {
            Some(StatsSkipReason::MergeCommit)
        } else {
            estimate_stats_cost(repo, &parent_sha, &commit_sha, &ignore_patterns)
                .ok()
                .and_then(|estimate| {
                    if should_skip_expensive_post_commit_stats(&estimate) {
                        Some(StatsSkipReason::Expensive(estimate))
                    } else {
                        None
                    }
                })
        };

        if skip_reason.is_none() {
            let diff_base = if parent_sha == "initial" {
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
            } else {
                &parent_sha
            };

            let diff_hunks =
                crate::commands::diff::get_diff_with_line_numbers(repo, diff_base, &commit_sha)?;

            let computed = stats_for_commit_stats_from_hunks(
                repo,
                &commit_sha,
                &ignore_patterns,
                &diff_hunks,
                Some(&authorship_log),
            )?;

            let hunks_json = crate::commands::diff::build_diff_artifacts_from_hunks(
                repo,
                diff_hunks,
                &commit_sha,
                Some(&authorship_log),
            )
            .ok()
            .and_then(|artifacts| serde_json::to_string(&artifacts.json_hunks).ok());

            // Record metrics only when we have full stats.
            record_commit_metrics(
                repo,
                &commit_sha,
                &parent_sha,
                &human_author,
                &authorship_note_str,
                &computed,
                &parent_working_log,
                hunks_json.as_deref(),
            );
            stats = Some(computed);
        }
    }

    if options.compute_stats && skip_reason.is_some() {
        match skip_reason.as_ref() {
            Some(StatsSkipReason::MergeCommit) => {
                tracing::debug!("Skipping post-commit stats for merge commit {}", commit_sha);
            }
            Some(StatsSkipReason::Expensive(estimate)) => {
                tracing::debug!(
                    "Skipping expensive post-commit stats for {} (files_with_additions={}, added_lines={}, deleted_lines={}, hunks={})",
                    commit_sha,
                    estimate.files_with_additions,
                    estimate.added_lines,
                    estimate.deleted_lines,
                    estimate.hunk_ranges
                );
            }
            None => {}
        }
    }

    // Write INITIAL file for uncommitted AI attributions (if any)
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo_storage.working_log_for_base_commit(&commit_sha)?;
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
        )?;
    }

    // // Clean up old working log
    repo_storage.delete_working_log_for_base_commit(&parent_sha)?;

    // Use Config::fresh() to support runtime config updates
    if !options.supress_output && !Config::fresh().is_quiet() {
        // Only print stats if we're in an interactive terminal and quiet mode is disabled
        let is_interactive = std::io::stdout().is_terminal();
        if let Some(stats) = stats.as_ref() {
            write_stats_to_terminal(stats, is_interactive);
        } else {
            match skip_reason.as_ref() {
                Some(StatsSkipReason::MergeCommit) => {
                    eprintln!(
                        "[git-ai] Skipped git-ai stats for merge commit {}.",
                        commit_sha
                    );
                }
                Some(StatsSkipReason::Expensive(estimate)) => {
                    eprintln!(
                        "[git-ai] Skipped git-ai stats for large commit (files_with_additions={}, added_lines={}, deleted_lines={}, hunks={}). Run `git-ai stats {}` to compute stats on demand.",
                        estimate.files_with_additions,
                        estimate.added_lines,
                        estimate.deleted_lines,
                        estimate.hunk_ranges,
                        commit_sha
                    );
                }
                None => {}
            }
        }
    }
    Ok((commit_sha.to_string(), authorship_log))
}

fn commit_tree_snapshot_for_files(
    repo: &Repository,
    commit_sha: &str,
    file_paths: &HashSet<String>,
) -> Result<HashMap<String, String>, GitAiError> {
    let requests = file_paths
        .iter()
        .map(|file_path| (commit_sha.to_string(), file_path.clone()))
        .collect::<Vec<_>>();
    let contents = batch_read_paths_at_treeishes(repo, &requests)?;
    let mut snapshot = HashMap::with_capacity(file_paths.len());
    for file_path in file_paths {
        snapshot.insert(
            file_path.clone(),
            contents
                .get(&(commit_sha.to_string(), file_path.clone()))
                .cloned()
                .unwrap_or_default(),
        );
    }

    Ok(snapshot)
}

fn recovery_committed_hunks(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    precomputed_parent_diff: Option<&crate::authorship::rewrite::DiffTreeResult>,
) -> Result<HashMap<String, Vec<crate::authorship::authorship_log::LineRange>>, GitAiError> {
    if let Some(diff) = precomputed_parent_diff {
        return Ok(
            crate::authorship::virtual_attribution::committed_hunks_from_diff_result(diff, None),
        );
    }

    let diff_base = if parent_sha == "initial" {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    } else {
        parent_sha
    };
    let added_lines = repo.diff_added_lines(diff_base, commit_sha, None)?;
    Ok(added_lines
        .into_iter()
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(path, lines)| {
            (
                path,
                crate::authorship::authorship_log::LineRange::compress_lines(&lines),
            )
        })
        .collect())
}

/// Amend-specific post-commit that merges blame-sourced attributions from the
/// original commit with persisted working-log checkpoint data.
pub fn post_commit_amend(
    repo: &Repository,
    original_commit: &str,
    amended_commit: &str,
    human_author: String,
) -> Result<(String, AuthorshipLog), GitAiError> {
    post_commit_amend_with_recovery_timestamps(
        repo,
        original_commit,
        amended_commit,
        human_author,
        None,
        None,
    )
}

pub(crate) fn post_commit_amend_with_recovery_timestamps(
    repo: &Repository,
    original_commit: &str,
    amended_commit: &str,
    human_author: String,
    recovery_file_timestamps: Option<&FileTimestampsByPath>,
    before_external_recovery: Option<&dyn Fn(&UnknownLinesByFile)>,
) -> Result<(String, AuthorshipLog), GitAiError> {
    let repo_storage = &repo.storage;
    let working_log = repo_storage.working_log_for_base_commit(original_commit)?;

    // Compute pathspecs: changed files in the amended commit + working log touched files
    let changed_files = repo.list_commit_files(amended_commit, None)?;
    let mut pathspecs: HashSet<String> = changed_files.into_iter().collect();
    let touched_files = working_log.all_touched_files()?;
    pathspecs.extend(touched_files);
    let initial_attributions_for_pathspecs = working_log.read_initial_attributions();
    for file_path in initial_attributions_for_pathspecs.files.keys() {
        pathspecs.insert(file_path.clone());
    }
    let pathspecs_vec: Vec<String> = pathspecs.iter().cloned().collect();
    let observed_snapshot = working_log.observed_file_snapshot()?;
    let mut final_state_snapshot =
        commit_tree_snapshot_for_files(repo, amended_commit, &pathspecs)?;
    final_state_snapshot.extend(observed_snapshot);

    // Check if original commit has existing authorship data
    let has_existing_data =
        crate::git::refs::get_reference_as_authorship_log_v3(repo, original_commit)
            .map(|log| {
                !log.metadata.prompts.is_empty()
                    || !log.metadata.humans.is_empty()
                    || !log.metadata.sessions.is_empty()
            })
            .unwrap_or(false);

    let working_va = crate::tokio_runtime::block_on(async {
        VirtualAttributions::from_working_log_for_commit_snapshot(
            repo.clone(),
            original_commit.to_string(),
            &pathspecs_vec,
            if has_existing_data {
                None
            } else {
                Some(human_author.clone())
            },
            None,
            &final_state_snapshot,
        )
        .await
    })?;

    // Resolve parent of the amended commit for diff base
    let amended_commit_obj = repo.find_commit(amended_commit.to_string())?;
    let parent_sha = if amended_commit_obj.parent_count()? > 0 {
        amended_commit_obj
            .parent(0)
            .map(|p| p.id())
            .unwrap_or_else(|_| "initial".to_string())
    } else {
        "initial".to_string()
    };

    let (mut authorship_log, initial_attributions, initial_file_contents) = working_va
        .to_authorship_log_and_initial_working_log(
            repo,
            &parent_sha,
            amended_commit,
            Some(&pathspecs),
            Some(&final_state_snapshot),
        )?;

    authorship_log.metadata.base_commit_sha = amended_commit.to_string();

    // Fill unattributed lines for background agents
    if !matches!(
        crate::authorship::background_agent::detect(),
        crate::authorship::background_agent::BackgroundAgent::None
            | crate::authorship::background_agent::BackgroundAgent::WithHooks { .. }
    ) {
        let diff_base = if parent_sha == "initial" {
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        } else {
            &parent_sha
        };
        if let Ok(added_lines) = repo.diff_added_lines(diff_base, amended_commit, None) {
            let committed_hunks: HashMap<
                String,
                Vec<crate::authorship::authorship_log::LineRange>,
            > = added_lines
                .into_iter()
                .filter(|(_, lines)| !lines.is_empty())
                .map(|(path, lines)| {
                    (
                        path,
                        crate::authorship::authorship_log::LineRange::compress_lines(&lines),
                    )
                })
                .collect();
            crate::authorship::background_agent::fill_unattributed_lines(
                &mut authorship_log,
                &committed_hunks,
                &human_author,
            );
        }
    }

    let recovery_hunks = recovery_committed_hunks(repo, &parent_sha, amended_commit, None)?;
    crate::authorship::attribution_recovery::recover_attribution(
        repo,
        &parent_sha,
        amended_commit,
        &human_author,
        &mut authorship_log,
        &recovery_hunks,
        AttributionRecoveryContext {
            file_timestamps: recovery_file_timestamps,
            before_external_recovery,
        },
    )?;
    authorship_log.metadata.base_commit_sha = amended_commit.to_string();

    // Preserve human/session metadata from the original commit's note
    if let Ok(original_log) =
        crate::git::refs::get_reference_as_authorship_log_v3(repo, original_commit)
    {
        for (id, record) in original_log.metadata.humans {
            authorship_log.metadata.humans.entry(id).or_insert(record);
        }
        let referenced_session_ids: HashSet<String> = authorship_log
            .attestations
            .iter()
            .flat_map(|fa| fa.entries.iter())
            .filter_map(|entry| {
                if entry.hash.starts_with("s_") {
                    Some(
                        entry
                            .hash
                            .split("::")
                            .next()
                            .unwrap_or(&entry.hash)
                            .to_string(),
                    )
                } else {
                    None
                }
            })
            .collect();
        for (id, record) in original_log.metadata.sessions {
            if referenced_session_ids.contains(&id) {
                authorship_log.metadata.sessions.entry(id).or_insert(record);
            }
        }
    }

    // Inject custom attributes
    let custom_attrs = Config::fresh().custom_attributes().clone();
    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
        for sr in authorship_log.metadata.sessions.values_mut() {
            sr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    let authorship_note_str = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
    notes_add(repo, amended_commit, &authorship_note_str)?;

    // Write INITIAL file for uncommitted attributions
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo_storage.working_log_for_base_commit(amended_commit)?;
        new_working_log.write_initial_attributions_with_contents(
            initial_attributions.files,
            initial_attributions.prompts,
            initial_attributions.humans,
            initial_file_contents,
            initial_attributions.sessions,
        )?;
    }

    // Clean up old working log
    repo_storage.delete_working_log_for_base_commit(original_commit)?;

    Ok((amended_commit.to_string(), authorship_log))
}

#[derive(Debug, Clone)]
enum StatsSkipReason {
    MergeCommit,
    Expensive(StatsCostEstimate),
}

#[doc(hidden)]
pub fn should_skip_expensive_post_commit_stats(estimate: &StatsCostEstimate) -> bool {
    estimate.hunk_ranges >= STATS_SKIP_MAX_HUNKS
        || estimate.added_lines >= STATS_SKIP_MAX_ADDED_LINES
        || estimate.files_with_additions >= STATS_SKIP_MAX_FILES_WITH_ADDITIONS
        || estimate.deleted_lines >= STATS_SKIP_MAX_DELETED_LINES
}

/// Public result of the stats cost estimate for a commit, used by the async
/// wrapper path to decide whether to skip expensive stats computation.
pub struct StatsSkipEstimate {
    should_skip: bool,
}

impl StatsSkipEstimate {
    pub fn should_skip(&self) -> bool {
        self.should_skip
    }
}

/// Estimate whether stats computation for `commit_sha` would be too expensive.
/// Resolves the parent commit automatically. Intended for callers outside the
/// normal post-commit flow (e.g. the async wrapper path).
pub fn estimate_stats_cost_for_head(
    repo: &Repository,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<StatsSkipEstimate, GitAiError> {
    let commit = repo.find_commit(commit_sha.to_string())?;
    let parent_sha = if commit.parent_count().unwrap_or(0) > 0 {
        commit
            .parent(0)
            .map(|p| p.id())
            .unwrap_or_else(|_| "initial".to_string())
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
    };
    estimate_stats_cost_for_commit_range(repo, &parent_sha, commit_sha, ignore_patterns)
}

pub fn estimate_stats_cost_for_commit_range(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<StatsSkipEstimate, GitAiError> {
    let estimate = estimate_stats_cost(repo, parent_sha, commit_sha, ignore_patterns)?;
    Ok(StatsSkipEstimate {
        should_skip: should_skip_expensive_post_commit_stats(&estimate),
    })
}

fn estimate_stats_cost(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<StatsCostEstimate, GitAiError> {
    let (mut added_lines_by_file, total_deleted_lines) =
        repo.diff_added_lines_with_deleted_count(parent_sha, commit_sha)?;
    let ignore_matcher = build_ignore_matcher(ignore_patterns);
    added_lines_by_file
        .retain(|file_path, _| !should_ignore_file_with_matcher(file_path, &ignore_matcher));

    let files_with_additions = added_lines_by_file
        .values()
        .filter(|lines| !lines.is_empty())
        .count();

    let mut added_lines = 0usize;
    let mut hunk_ranges = 0usize;

    for (_file, lines) in added_lines_by_file {
        if lines.is_empty() {
            continue;
        }
        added_lines += lines.len();
        hunk_ranges += count_line_ranges(&lines);
    }

    Ok(StatsCostEstimate {
        files_with_additions,
        added_lines,
        hunk_ranges,
        deleted_lines: total_deleted_lines,
    })
}

#[doc(hidden)]
pub fn count_line_ranges(lines: &[u32]) -> usize {
    if lines.is_empty() {
        return 0;
    }

    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut ranges = 1usize;
    let mut prev = sorted[0];
    for &line in &sorted[1..] {
        if line != prev + 1 {
            ranges += 1;
        }
        prev = line;
    }
    ranges
}

/// Record metrics for a committed change.
/// This is a best-effort operation - failures are silently ignored.
#[allow(clippy::too_many_arguments)]
fn record_commit_metrics(
    repo: &Repository,
    commit_sha: &str,
    parent_sha: &str,
    human_author: &str,
    authorship_note: &str,
    stats: &crate::authorship::stats::CommitStats,
    checkpoints: &[Checkpoint],
    hunks_json: Option<&str>,
) {
    use crate::metrics::{CommittedValues, EventAttributes, record};

    // Never emit telemetry for mock_ai (test preset).  If every tool in the
    // breakdown is mock_ai the entire committed event is test data.
    let only_mock_ai = !stats.tool_model_breakdown.is_empty()
        && stats
            .tool_model_breakdown
            .keys()
            .all(|k| k.starts_with("mock_ai::"));
    if only_mock_ai {
        return;
    }

    // Subtract mock_ai contributions from the aggregates so the "all" entry
    // only reflects real tools.
    let mut agg_ai = stats.ai_additions;
    let mut agg_accepted = stats.ai_accepted;
    for (key, ts) in &stats.tool_model_breakdown {
        if key.starts_with("mock_ai::") {
            agg_ai = agg_ai.saturating_sub(ts.ai_additions);
            agg_accepted = agg_accepted.saturating_sub(ts.ai_accepted);
        }
    }

    // Build parallel arrays: index 0 = "all" (aggregate), index 1+ = per tool/model
    let mut tool_model_pairs: Vec<String> = vec!["all".to_string()];
    let mut ai_additions: Vec<u32> = vec![agg_ai];
    let mut ai_accepted: Vec<u32> = vec![agg_accepted];

    // Add per-tool/model breakdown, skipping mock_ai (test preset)
    for (tool_model, tool_stats) in &stats.tool_model_breakdown {
        if tool_model.starts_with("mock_ai::") {
            continue;
        }
        tool_model_pairs.push(tool_model.clone());
        ai_additions.push(tool_stats.ai_additions);
        ai_accepted.push(tool_stats.ai_accepted);
    }

    // Build values with all stats
    let values = CommittedValues::new()
        .human_additions(stats.human_additions)
        .git_diff_deleted_lines(stats.git_diff_deleted_lines)
        .git_diff_added_lines(stats.git_diff_added_lines)
        .tool_model_pairs(tool_model_pairs)
        .ai_additions(ai_additions)
        .ai_accepted(ai_accepted);

    // Add first checkpoint timestamp (null if no checkpoints)
    let values = if let Some(first) = checkpoints.first() {
        values.first_checkpoint_ts(first.timestamp)
    } else {
        values.first_checkpoint_ts_null()
    };

    // Add commit subject and body
    let values = if let Ok(commit) = repo.find_commit(commit_sha.to_string()) {
        let subject = commit.summary().unwrap_or_default();
        let values = values.commit_subject(subject);
        let body = commit.body().unwrap_or_default();
        if body.is_empty() {
            values.commit_body_null()
        } else {
            values.commit_body(body)
        }
    } else {
        values.commit_subject_null().commit_body_null()
    };

    let values = values.authorship_note(authorship_note);

    let values = if let Some(hunks) = hunks_json {
        values.hunks(hunks)
    } else {
        values.hunks_null()
    };

    // Build attributes - start with version and extract session_id from first AI checkpoint
    // session_id links this commit to the AI agent conversation that produced it
    // Note: session_id removed from committed events - commits can contain code from multiple AI sessions
    let mut attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"));

    attrs = attrs
        .author(human_author)
        .commit_sha(commit_sha)
        .base_commit_sha(parent_sha);

    // Get repo URL from default remote
    if let Ok(Some(remote_name)) = repo.get_default_remote()
        && let Ok(remotes) = repo.remotes_with_urls()
        && let Some((_, url)) = remotes.into_iter().find(|(n, _)| n == &remote_name)
        && let Ok(normalized) = crate::repo_url::normalize_repo_url(&url)
    {
        attrs = attrs.repo_url(normalized);
    }

    // Get current branch
    if let Ok(head_ref) = repo.head()
        && let Ok(short_branch) = head_ref.shorthand()
    {
        attrs = attrs.branch(short_branch);
    }

    // Attach custom attributes using Config::fresh() to support runtime config updates
    attrs = attrs.custom_attributes_map(Config::fresh().custom_attributes());

    // Record the metric
    record(values, attrs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_line_ranges_handles_scattered_and_contiguous_lines() {
        assert_eq!(count_line_ranges(&[]), 0);
        assert_eq!(count_line_ranges(&[1]), 1);
        assert_eq!(count_line_ranges(&[1, 2, 3]), 1);
        assert_eq!(count_line_ranges(&[1, 3, 5]), 3);
        // Includes unsorted and duplicate values.
        assert_eq!(count_line_ranges(&[5, 3, 3, 4, 10]), 2);
    }

    #[test]
    fn test_should_skip_expensive_post_commit_stats_thresholds() {
        let below_threshold = StatsCostEstimate {
            files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS - 1,
            added_lines: STATS_SKIP_MAX_ADDED_LINES - 1,
            hunk_ranges: STATS_SKIP_MAX_HUNKS - 1,
            deleted_lines: STATS_SKIP_MAX_DELETED_LINES - 1,
        };
        assert!(!should_skip_expensive_post_commit_stats(&below_threshold));

        let by_hunks = StatsCostEstimate {
            files_with_additions: 1,
            added_lines: 1,
            hunk_ranges: STATS_SKIP_MAX_HUNKS,
            deleted_lines: 0,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_hunks));

        let by_added_lines = StatsCostEstimate {
            files_with_additions: 1,
            added_lines: STATS_SKIP_MAX_ADDED_LINES,
            hunk_ranges: 1,
            deleted_lines: 0,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_added_lines));

        let by_files = StatsCostEstimate {
            files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
            added_lines: 1,
            hunk_ranges: 1,
            deleted_lines: 0,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_files));

        let by_deleted_lines = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: STATS_SKIP_MAX_DELETED_LINES,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_deleted_lines));
    }

    #[test]
    fn test_count_line_ranges_single_element() {
        assert_eq!(count_line_ranges(&[42]), 1);
    }

    #[test]
    fn test_count_line_ranges_all_contiguous() {
        assert_eq!(count_line_ranges(&[1, 2, 3, 4, 5]), 1);
    }

    #[test]
    fn test_count_line_ranges_all_scattered() {
        assert_eq!(count_line_ranges(&[1, 10, 20, 30]), 4);
    }

    #[test]
    fn test_count_line_ranges_duplicates() {
        assert_eq!(count_line_ranges(&[5, 5, 5]), 1);
    }

    #[test]
    fn test_count_line_ranges_unsorted() {
        // After sort+dedup: [1, 2, 5, 6, 10] -> ranges: [1,2], [5,6], [10]
        assert_eq!(count_line_ranges(&[10, 5, 6, 1, 2]), 3);
    }

    #[test]
    fn test_count_line_ranges_two_ranges() {
        assert_eq!(count_line_ranges(&[1, 2, 3, 10, 11, 12]), 2);
    }

    #[test]
    fn test_should_skip_stats_exactly_at_thresholds() {
        // Exactly at the hunks threshold alone should trigger skip.
        let at_hunks = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: STATS_SKIP_MAX_HUNKS,
            deleted_lines: 0,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_hunks),
            "Exactly at hunk threshold should skip"
        );

        // Exactly at added-lines threshold alone should trigger skip.
        let at_added = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: STATS_SKIP_MAX_ADDED_LINES,
            hunk_ranges: 0,
            deleted_lines: 0,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_added),
            "Exactly at added-lines threshold should skip"
        );

        // Exactly at files-with-additions threshold alone should trigger skip.
        let at_files = StatsCostEstimate {
            files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: 0,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_files),
            "Exactly at files-with-additions threshold should skip"
        );

        // Exactly at deleted-lines threshold alone should trigger skip.
        let at_deleted = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: STATS_SKIP_MAX_DELETED_LINES,
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_deleted),
            "Exactly at deleted-lines threshold should skip"
        );

        // All at zero should NOT skip.
        let all_zero = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: 0,
            deleted_lines: 0,
        };
        assert!(
            !should_skip_expensive_post_commit_stats(&all_zero),
            "All zero values should not skip"
        );
    }
}
