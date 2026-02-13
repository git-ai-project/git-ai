use crate::api::{ApiClient, ApiContext};
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::ignore::{
    IgnoreMatcher, build_ignore_matcher, effective_ignore_patterns, should_ignore_file_with_matcher,
};
use crate::authorship::prompt_utils::{PromptUpdateResult, update_prompt_from_tool};
use crate::authorship::secrets::{redact_secrets_from_prompts, strip_prompt_messages};
use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::{Checkpoint, CheckpointKind, WorkingLogEntry};
use crate::config::{Config, PromptStorageMode};
use crate::error::GitAiError;
use crate::git::refs::notes_add;
use crate::git::repository::Repository;
use crate::utils::debug_log;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;
use std::time::Instant;

/// Skip expensive post-commit stats when this threshold is exceeded.
/// High hunk density is the strongest predictor of slow diff_ai_accepted_stats.
const STATS_SKIP_MAX_HUNKS: usize = 1000;
/// Skip expensive stats for very large net additions even if hunks are moderate.
const STATS_SKIP_MAX_ADDED_LINES: usize = 6000;
/// Skip expensive stats for extremely wide commits touching many added-line files.
const STATS_SKIP_MAX_FILES_WITH_ADDITIONS: usize = 200;

#[derive(Debug, Clone, Copy)]
struct StatsCostEstimate {
    files_with_additions: usize,
    added_lines: usize,
    hunk_ranges: usize,
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
    let post_commit_start = Instant::now();
    // Use base_commit parameter if provided, otherwise use "initial" for empty repos
    // This matches the convention in checkpoint.rs
    let parent_sha = base_commit.unwrap_or_else(|| "initial".to_string());

    // Initialize the new storage system
    let repo_storage = &repo.storage;
    let working_log = repo_storage.working_log_for_base_commit(&parent_sha);
    let squash_precommit_skipped = working_log.was_squash_precommit_skipped();
    let initial_read_start = Instant::now();
    let initial_attributions_for_pathspecs = working_log.read_initial_attributions();
    debug_log(&format!(
        "[BENCHMARK] post-commit: read initial attributions took {:?}",
        initial_read_start.elapsed()
    ));

    // Pull all working log entries from the parent commit.
    // For squash commits where pre-commit checkpoint was intentionally skipped,
    // only INITIAL data is relevant and we can avoid checkpoint I/O entirely.
    let mut parent_working_log = if squash_precommit_skipped {
        Vec::new()
    } else {
        working_log.read_all_checkpoints()?
    };

    if !squash_precommit_skipped {
        // Update prompts/transcripts to their latest versions and persist to disk
        // Do this BEFORE filtering so that all checkpoints (including untracked files) are updated
        update_prompts_to_latest(&mut parent_working_log)?;

        // Batch upsert all prompts to database after refreshing (non-fatal if it fails)
        if let Err(e) = batch_upsert_prompts_to_db(&parent_working_log, &working_log, &commit_sha) {
            debug_log(&format!(
                "[Warning] Failed to batch upsert prompts to database: {}",
                e
            ));
            crate::observability::log_error(
                &e,
                Some(serde_json::json!({
                    "operation": "post_commit_batch_upsert",
                    "commit_sha": commit_sha
                })),
            );
        }

        working_log.write_all_checkpoints(&parent_working_log)?;
    }

    // Create VirtualAttributions from working log (fast path - no blame).
    // Post-commit conversion only needs line-level attributions, so use the
    // lightweight loader to avoid eagerly reading full file contents.
    let va_start = Instant::now();
    let working_va = if squash_precommit_skipped {
        VirtualAttributions::from_initial_only_line_only(
            repo.clone(),
            parent_sha.clone(),
            Some(human_author.clone()),
        )?
    } else {
        VirtualAttributions::from_just_working_log_line_only(
            repo.clone(),
            parent_sha.clone(),
            Some(human_author.clone()),
        )?
    };
    debug_log(&format!(
        "[BENCHMARK] post-commit: load working VA took {:?}",
        va_start.elapsed()
    ));

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
    for file_path in initial_attributions_for_pathspecs.files.keys() {
        pathspecs.insert(file_path.clone());
    }

    // Split VirtualAttributions into committed (authorship log) and uncommitted (INITIAL)
    // Fast path: if there are no relevant worktree changes after commit, we can skip
    // expensive unstaged diff processing and convert directly from committed hunks.
    let status_start = Instant::now();
    let has_relevant_worktree_changes =
        !pathspecs.is_empty() && !repo.status(Some(&pathspecs), false)?.is_empty();
    debug_log(&format!(
        "[BENCHMARK] post-commit: status scan took {:?} (has_changes={})",
        status_start.elapsed(),
        has_relevant_worktree_changes
    ));

    let conversion_start = Instant::now();
    let (mut authorship_log, initial_attributions) = if has_relevant_worktree_changes {
        working_va.to_authorship_log_and_initial_working_log(
            repo,
            &parent_sha,
            &commit_sha,
            Some(&pathspecs),
        )?
    } else {
        let authorship_log = working_va.to_authorship_log_index_only(
            repo,
            &parent_sha,
            &commit_sha,
            Some(&pathspecs),
        )?;
        let initial_attributions = crate::git::repo_storage::InitialAttributions {
            files: HashMap::new(),
            prompts: HashMap::new(),
        };
        (authorship_log, initial_attributions)
    };
    debug_log(&format!(
        "[BENCHMARK] post-commit: attribution conversion took {:?}",
        conversion_start.elapsed()
    ));

    authorship_log.metadata.base_commit_sha = commit_sha.clone();

    let prompt_policy_start = Instant::now();
    // Handle prompts based on effective prompt storage mode for this repository
    // The effective mode considers include/exclude lists and fallback settings
    let effective_storage = Config::get().effective_prompt_storage(&Some(repo.clone()));
    let has_prompt_messages = authorship_log
        .metadata
        .prompts
        .values()
        .any(|prompt| !prompt.messages.is_empty());

    match effective_storage {
        PromptStorageMode::Local => {
            // Local only: strip all messages from notes (they stay in sqlite only)
            if has_prompt_messages {
                strip_prompt_messages(&mut authorship_log.metadata.prompts);
            }
        }
        PromptStorageMode::Notes => {
            // Store in notes: redact secrets but keep messages in notes
            if has_prompt_messages {
                let count = redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
                if count > 0 {
                    debug_log(&format!("Redacted {} secrets from prompts", count));
                }
            }
        }
        PromptStorageMode::Default => {
            // "default" - attempt CAS upload, NEVER keep messages in notes
            if has_prompt_messages {
                // Check conditions for CAS upload:
                // - user is logged in OR using custom API URL
                // - squash fast-path is always false (prompts are inherited)
                let should_enqueue_cas = if squash_precommit_skipped {
                    false
                } else {
                    let context = ApiContext::new(None);
                    let client = ApiClient::new(context);
                    let using_custom_api =
                        Config::get().api_base_url() != crate::config::DEFAULT_API_BASE_URL;
                    client.is_logged_in() || using_custom_api
                };

                if should_enqueue_cas {
                    // Redact secrets before uploading to CAS
                    let redaction_count =
                        redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
                    if redaction_count > 0 {
                        debug_log(&format!(
                            "Redacted {} secrets from prompts before CAS upload",
                            redaction_count
                        ));
                    }

                    if let Err(e) =
                        enqueue_prompt_messages_to_cas(repo, &mut authorship_log.metadata.prompts)
                    {
                        debug_log(&format!(
                            "[Warning] Failed to enqueue prompt messages to CAS: {}",
                            e
                        ));
                        // Enqueue failed - still strip messages (never keep in notes for "default")
                        strip_prompt_messages(&mut authorship_log.metadata.prompts);
                    }
                    // Success: enqueue function already cleared messages
                } else {
                    // Not enqueueing - strip messages (never keep in notes for "default")
                    strip_prompt_messages(&mut authorship_log.metadata.prompts);
                }
            }
        }
    }
    debug_log(&format!(
        "[BENCHMARK] post-commit: prompt policy handling took {:?}",
        prompt_policy_start.elapsed()
    ));

    // Serialize the authorship log
    let serialize_start = Instant::now();
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;
    debug_log(&format!(
        "[BENCHMARK] post-commit: serialize authorship took {:?}",
        serialize_start.elapsed()
    ));

    let notes_start = Instant::now();
    notes_add(repo, &commit_sha, &authorship_json)?;
    debug_log(&format!(
        "[BENCHMARK] post-commit: notes_add took {:?}",
        notes_start.elapsed()
    ));

    // Compute stats once (needed for both metrics and terminal output), unless preflight
    // estimate predicts this would be too expensive for the commit hook path.
    let mut stats: Option<crate::authorship::stats::CommitStats> = None;
    let is_merge_commit = repo
        .find_commit(commit_sha.clone())
        .map(|commit| commit.parent_count().unwrap_or(0) > 1)
        .unwrap_or(false);
    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    let stats_preflight_start = Instant::now();
    let skip_reason = if is_merge_commit {
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
    debug_log(&format!(
        "[BENCHMARK] post-commit: stats preflight took {:?}",
        stats_preflight_start.elapsed()
    ));

    if skip_reason.is_none() {
        let computed = stats_for_commit_stats(repo, &commit_sha, &ignore_patterns)?;
        // Record metrics only when we have full stats.
        record_commit_metrics(
            repo,
            &commit_sha,
            &parent_sha,
            &human_author,
            &authorship_log,
            &computed,
            &parent_working_log,
        );
        stats = Some(computed);
    } else {
        match skip_reason.as_ref() {
            Some(StatsSkipReason::MergeCommit) => {
                debug_log(&format!(
                    "Skipping post-commit stats for merge commit {}",
                    commit_sha
                ));
            }
            Some(StatsSkipReason::Expensive(estimate)) => {
                debug_log(&format!(
                    "Skipping expensive post-commit stats for {} (files_with_additions={}, added_lines={}, hunks={})",
                    commit_sha,
                    estimate.files_with_additions,
                    estimate.added_lines,
                    estimate.hunk_ranges
                ));
            }
            None => {}
        }
    }

    // Write INITIAL file for uncommitted AI attributions (if any)
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo_storage.working_log_for_base_commit(&commit_sha);
        new_working_log
            .write_initial_attributions(initial_attributions.files, initial_attributions.prompts)?;
    }

    // // Clean up old working log
    let cleanup_start = Instant::now();
    repo_storage.delete_working_log_for_base_commit(&parent_sha)?;
    debug_log(&format!(
        "[BENCHMARK] post-commit: cleanup old working log took {:?}",
        cleanup_start.elapsed()
    ));
    debug_log(&format!(
        "[BENCHMARK] post-commit: total post_commit duration {:?}",
        post_commit_start.elapsed()
    ));

    if !supress_output && !Config::get().is_quiet() {
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
                        "[git-ai] Skipped git-ai stats for large commit (files_with_additions={}, added_lines={}, hunks={}). Run `git-ai stats {}` to compute stats on demand.",
                        estimate.files_with_additions,
                        estimate.added_lines,
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

#[derive(Debug, Clone)]
enum StatsSkipReason {
    MergeCommit,
    Expensive(StatsCostEstimate),
}

fn should_skip_expensive_post_commit_stats(estimate: &StatsCostEstimate) -> bool {
    estimate.hunk_ranges >= STATS_SKIP_MAX_HUNKS
        || estimate.added_lines >= STATS_SKIP_MAX_ADDED_LINES
        || estimate.files_with_additions >= STATS_SKIP_MAX_FILES_WITH_ADDITIONS
}

fn estimate_stats_cost(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    ignore_patterns: &[String],
) -> Result<StatsCostEstimate, GitAiError> {
    let ignore_matcher = build_ignore_matcher(ignore_patterns);
    // Use a cheap --numstat preflight first. On large commits this avoids parsing full
    // patch hunks just to decide we should skip expensive stats.
    let (files_with_additions, added_lines) =
        get_numstat_added_lines(repo, parent_sha, commit_sha, &ignore_matcher)?;

    if files_with_additions >= STATS_SKIP_MAX_FILES_WITH_ADDITIONS
        || added_lines >= STATS_SKIP_MAX_ADDED_LINES
    {
        return Ok(StatsCostEstimate {
            files_with_additions,
            added_lines,
            hunk_ranges: 0,
        });
    }

    // Only compute hunk density when needed (smaller commits near threshold).
    let mut added_lines_by_file = repo.diff_added_lines(parent_sha, commit_sha, None)?;
    added_lines_by_file
        .retain(|file_path, _| !should_ignore_file_with_matcher(file_path, &ignore_matcher));
    let files_with_additions = added_lines_by_file
        .values()
        .filter(|lines| !lines.is_empty())
        .count();
    let added_lines = added_lines_by_file.values().map(std::vec::Vec::len).sum();
    let hunk_ranges = added_lines_by_file
        .values()
        .filter(|lines| !lines.is_empty())
        .map(|lines| count_line_ranges(lines))
        .sum();

    Ok(StatsCostEstimate {
        files_with_additions,
        added_lines,
        hunk_ranges,
    })
}

fn count_line_ranges(lines: &[u32]) -> usize {
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

fn get_numstat_added_lines(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    ignore_matcher: &IgnoreMatcher,
) -> Result<(usize, usize), GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("diff".to_string());
    args.push("--numstat".to_string());
    args.push(parent_sha.to_string());
    args.push(commit_sha.to_string());

    let output = crate::git::repository::exec_git(&args)?;
    let stdout = String::from_utf8(output.stdout)?;

    let mut files_with_additions = 0usize;
    let mut added_lines = 0usize;

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        if should_ignore_file_with_matcher(parts[2], ignore_matcher) {
            continue;
        }
        if let Ok(added) = parts[0].parse::<usize>()
            && added > 0
        {
            files_with_additions += 1;
            added_lines += added;
        }
    }

    Ok((files_with_additions, added_lines))
}

/// Update prompts/transcripts in working log checkpoints to their latest versions.
/// This helps prevent race conditions where we miss the last message in a conversation.
///
/// For each unique prompt/conversation (identified by agent_id), only the LAST checkpoint
/// with that agent_id is updated. This prevents duplicating the same full transcript
/// across multiple checkpoints when only the final version matters.
fn update_prompts_to_latest(checkpoints: &mut [Checkpoint]) -> Result<(), GitAiError> {
    // Group checkpoints by agent ID (tool + id), tracking indices
    let mut agent_checkpoint_indices: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, checkpoint) in checkpoints.iter().enumerate() {
        if let Some(agent_id) = &checkpoint.agent_id {
            let key = format!("{}:{}", agent_id.tool, agent_id.id);
            agent_checkpoint_indices.entry(key).or_default().push(idx);
        }
    }

    // For each unique agent/conversation, update only the LAST checkpoint
    for (_agent_key, indices) in agent_checkpoint_indices {
        if indices.is_empty() {
            continue;
        }

        // Get the last checkpoint index for this agent
        let last_idx = *indices.last().unwrap();
        let checkpoint = &checkpoints[last_idx];

        if let Some(agent_id) = &checkpoint.agent_id {
            // Use shared update logic from prompt_updater module
            let result = update_prompt_from_tool(
                &agent_id.tool,
                &agent_id.id,
                checkpoint.agent_metadata.as_ref(),
                &agent_id.model,
            );

            // Apply the update to the last checkpoint only
            match result {
                PromptUpdateResult::Updated(latest_transcript, latest_model) => {
                    let checkpoint = &mut checkpoints[last_idx];
                    checkpoint.transcript = Some(latest_transcript);
                    if let Some(agent_id) = &mut checkpoint.agent_id {
                        agent_id.model = latest_model;
                    }
                }
                PromptUpdateResult::Unchanged => {
                    // No update available, keep existing transcript
                }
                PromptUpdateResult::Failed(_e) => {
                    // Error already logged in update_prompt_from_tool
                    // Continue processing other checkpoints
                }
            }
        }
    }

    Ok(())
}

/// Batch upsert all prompts from checkpoints to the internal database.
/// For each unique agent_id (tool:id), only the LAST checkpoint is inserted.
/// This mirrors the deduplication logic in update_prompts_to_latest().
fn batch_upsert_prompts_to_db(
    checkpoints: &[Checkpoint],
    working_log: &crate::git::repo_storage::PersistedWorkingLog,
    commit_sha: &str,
) -> Result<(), GitAiError> {
    use crate::authorship::internal_db::{InternalDatabase, PromptDbRecord};

    let workdir = working_log.repo_workdir.to_string_lossy().to_string();

    // Group checkpoints by agent_id, keeping track of the LAST index for each.
    // This mirrors the logic in update_prompts_to_latest().
    let mut last_checkpoint_by_agent: HashMap<String, usize> = HashMap::new();

    for (idx, checkpoint) in checkpoints.iter().enumerate() {
        if checkpoint.kind == CheckpointKind::Human {
            continue;
        }
        if let Some(agent_id) = &checkpoint.agent_id {
            let key = format!("{}:{}", agent_id.tool, agent_id.id);
            // Always update to the latest index (overwrites previous)
            last_checkpoint_by_agent.insert(key, idx);
        }
    }

    // Only create records for the LAST checkpoint of each agent_id
    // Note: from_checkpoint now uses message timestamps for created_at/updated_at
    let mut records = Vec::new();
    for (_agent_key, idx) in last_checkpoint_by_agent {
        let checkpoint = &checkpoints[idx];
        if let Some(record) = PromptDbRecord::from_checkpoint(
            checkpoint,
            Some(workdir.clone()),
            Some(commit_sha.to_string()),
        ) {
            records.push(record);
        }
    }

    if records.is_empty() {
        return Ok(());
    }

    let db = InternalDatabase::global()?;
    let mut db_guard = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    db_guard.batch_upsert_prompts(&records)?;

    Ok(())
}

/// Enqueue prompt messages to CAS for external storage.
/// For each prompt with non-empty messages:
/// - Serialize messages to JSON
/// - Enqueue to CAS (returns hash)
/// - Set messages_url (format: {api_base_url}/cas/{hash}) and clear messages
fn enqueue_prompt_messages_to_cas(
    repo: &Repository,
    prompts: &mut std::collections::BTreeMap<
        String,
        crate::authorship::authorship_log::PromptRecord,
    >,
) -> Result<(), GitAiError> {
    use crate::authorship::internal_db::InternalDatabase;

    let db = InternalDatabase::global()?;
    let mut db_lock = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    // CAS metadata for prompt messages
    let mut metadata = HashMap::new();
    metadata.insert("api_version".to_string(), "v1".to_string());
    metadata.insert("kind".to_string(), "prompt".to_string());

    // Get repo URL from default remote
    let repo_url = repo
        .get_default_remote()
        .ok()
        .flatten()
        .and_then(|remote_name| {
            repo.remotes_with_urls().ok().and_then(|remotes| {
                remotes
                    .into_iter()
                    .find(|(name, _)| name == &remote_name)
                    .map(|(_, url)| url)
            })
        });

    if let Some(url) = repo_url
        && let Ok(normalized) = crate::repo_url::normalize_repo_url(&url)
    {
        metadata.insert("repo_url".to_string(), normalized);
    }

    // Get API base URL for constructing messages_url
    let api_base_url = Config::get().api_base_url();

    for (_key, prompt) in prompts.iter_mut() {
        if !prompt.messages.is_empty() {
            // Wrap messages in CasMessagesObject and serialize to JSON
            let messages_obj = crate::api::types::CasMessagesObject {
                messages: prompt.messages.clone(),
            };
            let messages_json = serde_json::to_value(&messages_obj)
                .map_err(|e| GitAiError::Generic(format!("Failed to serialize messages: {}", e)))?;

            // Enqueue to CAS (returns hash)
            let hash = db_lock.enqueue_cas_object(&messages_json, Some(&metadata))?;

            // Set full URL and clear messages
            prompt.messages_url = Some(format!("{}/cas/{}", api_base_url, hash));
            prompt.messages.clear();
        }
    }

    Ok(())
}

/// Record metrics for a committed change.
/// This is a best-effort operation - failures are silently ignored.
fn record_commit_metrics(
    repo: &Repository,
    commit_sha: &str,
    parent_sha: &str,
    human_author: &str,
    _authorship_log: &AuthorshipLog,
    stats: &crate::authorship::stats::CommitStats,
    checkpoints: &[Checkpoint],
) {
    use crate::metrics::{CommittedValues, EventAttributes, record};

    // Build parallel arrays: index 0 = "all" (aggregate), index 1+ = per tool/model
    let mut tool_model_pairs: Vec<String> = vec!["all".to_string()];
    let mut mixed_additions: Vec<u32> = vec![stats.mixed_additions];
    let mut ai_additions: Vec<u32> = vec![stats.ai_additions];
    let mut ai_accepted: Vec<u32> = vec![stats.ai_accepted];
    let mut total_ai_additions: Vec<u32> = vec![stats.total_ai_additions];
    let mut total_ai_deletions: Vec<u32> = vec![stats.total_ai_deletions];
    let mut time_waiting_for_ai: Vec<u64> = vec![stats.time_waiting_for_ai];

    // Add per-tool/model breakdown
    for (tool_model, tool_stats) in &stats.tool_model_breakdown {
        tool_model_pairs.push(tool_model.clone());
        mixed_additions.push(tool_stats.mixed_additions);
        ai_additions.push(tool_stats.ai_additions);
        ai_accepted.push(tool_stats.ai_accepted);
        total_ai_additions.push(tool_stats.total_ai_additions);
        total_ai_deletions.push(tool_stats.total_ai_deletions);
        time_waiting_for_ai.push(tool_stats.time_waiting_for_ai);
    }

    // Build values with all stats
    let values = CommittedValues::new()
        .human_additions(stats.human_additions)
        .git_diff_deleted_lines(stats.git_diff_deleted_lines)
        .git_diff_added_lines(stats.git_diff_added_lines)
        .tool_model_pairs(tool_model_pairs)
        .mixed_additions(mixed_additions)
        .ai_additions(ai_additions)
        .ai_accepted(ai_accepted)
        .total_ai_additions(total_ai_additions)
        .total_ai_deletions(total_ai_deletions)
        .time_waiting_for_ai(time_waiting_for_ai);

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

    // Build attributes - start with version
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

    // Record the metric
    record(values, attrs);
}

#[cfg(test)]
mod tests {
    use super::{
        STATS_SKIP_MAX_ADDED_LINES, STATS_SKIP_MAX_FILES_WITH_ADDITIONS, STATS_SKIP_MAX_HUNKS,
        StatsCostEstimate, checkpoint_entry_requires_post_processing, count_line_ranges,
        should_skip_expensive_post_commit_stats,
    };
    use crate::authorship::working_log::{Checkpoint, CheckpointKind, WorkingLogEntry};
    use crate::git::test_utils::TmpRepo;

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
        };
        assert!(!should_skip_expensive_post_commit_stats(&below_threshold));

        let by_hunks = StatsCostEstimate {
            files_with_additions: 1,
            added_lines: 1,
            hunk_ranges: STATS_SKIP_MAX_HUNKS,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_hunks));

        let by_added_lines = StatsCostEstimate {
            files_with_additions: 1,
            added_lines: STATS_SKIP_MAX_ADDED_LINES,
            hunk_ranges: 1,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_added_lines));

        let by_files = StatsCostEstimate {
            files_with_additions: STATS_SKIP_MAX_FILES_WITH_ADDITIONS,
            added_lines: 1,
            hunk_ranges: 1,
        };
        assert!(should_skip_expensive_post_commit_stats(&by_files));
    }

    #[test]
    fn test_checkpoint_entry_requires_post_processing_handles_human_and_ai_paths() {
        let empty_human_checkpoint = Checkpoint::new(
            CheckpointKind::Human,
            String::new(),
            "human".to_string(),
            vec![],
        );
        let empty_entry = WorkingLogEntry::new(
            "file.txt".to_string(),
            "blob".to_string(),
            vec![],
            vec![],
        );
        assert!(
            !checkpoint_entry_requires_post_processing(&empty_human_checkpoint, &empty_entry),
            "human entry with no AI attribution should be skipped"
        );

        let ai_line_entry = WorkingLogEntry::new(
            "file.txt".to_string(),
            "blob".to_string(),
            vec![],
            vec![crate::authorship::attribution_tracker::LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "mock_ai".to_string(),
                overrode: None,
            }],
        );
        assert!(
            checkpoint_entry_requires_post_processing(&empty_human_checkpoint, &ai_line_entry),
            "human entry with AI line attribution should be processed"
        );

        let overridden_entry = WorkingLogEntry::new(
            "file.txt".to_string(),
            "blob".to_string(),
            vec![],
            vec![crate::authorship::attribution_tracker::LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: CheckpointKind::Human.to_str(),
                overrode: Some("mock_ai".to_string()),
            }],
        );
        assert!(
            checkpoint_entry_requires_post_processing(&empty_human_checkpoint, &overridden_entry),
            "human overrides must be processed"
        );

        let ai_checkpoint = Checkpoint::new(
            CheckpointKind::AiAgent,
            String::new(),
            "ai".to_string(),
            vec![],
        );
        assert!(
            checkpoint_entry_requires_post_processing(&ai_checkpoint, &empty_entry),
            "all AI checkpoints should be processed"
        );
    }

    #[test]
    fn test_post_commit_empty_repo_with_checkpoint() {
        // Create an empty repo (no commits yet)
        let tmp_repo = TmpRepo::new().unwrap();

        // Create a file and checkpoint it (no commit yet)
        let mut file = tmp_repo
            .write_file("test.txt", "Hello, world!\n", false)
            .unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        // Make a change and checkpoint again
        file.append("Second line\n").unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();

        // Now make the first commit (empty repo case: base_commit is None)
        let result = tmp_repo.commit_with_message("Initial commit");

        // Should not panic or error - this is the key test
        // The main goal is to ensure empty repos (base_commit=None) don't cause errors
        assert!(
            result.is_ok(),
            "post_commit should handle empty repo (base_commit=None) without errors"
        );

        // The authorship log is created successfully (even if empty for human-only checkpoints)
        let _authorship_log = result.unwrap();
    }

    #[test]
    fn test_post_commit_empty_repo_no_checkpoint() {
        // Create an empty repo (no commits yet)
        let tmp_repo = TmpRepo::new().unwrap();

        // Create a file without checkpointing
        tmp_repo
            .write_file("test.txt", "Hello, world!\n", false)
            .unwrap();

        // Make the first commit with no prior checkpoints
        let result = tmp_repo.commit_with_message("Initial commit");

        // Should not panic or error even with no working log
        assert!(
            result.is_ok(),
            "post_commit should handle empty repo with no checkpoints without errors"
        );

        let authorship_log = result.unwrap();

        // The authorship log should be created but empty (no AI checkpoints)
        // All changes will be attributed to the human author
        assert!(
            authorship_log.attestations.is_empty(),
            "Should have empty attestations when no checkpoints exist"
        );
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
        };
        assert!(
            should_skip_expensive_post_commit_stats(&at_files),
            "Exactly at files-with-additions threshold should skip"
        );

        // All at zero should NOT skip.
        let all_zero = StatsCostEstimate {
            files_with_additions: 0,
            added_lines: 0,
            hunk_ranges: 0,
        };
        assert!(
            !should_skip_expensive_post_commit_stats(&all_zero),
            "All zero values should not skip"
        );
    }

    #[test]
    fn test_post_commit_utf8_filename_with_ai_attribution() {
        // Create a repo with an initial commit
        let tmp_repo = TmpRepo::new().unwrap();

        // Create initial file and commit
        tmp_repo.write_file("README.md", "# Test\n", true).unwrap();
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .unwrap();
        tmp_repo.commit_with_message("Initial commit").unwrap();

        // Create a file with Chinese characters in the filename
        let chinese_filename = "中文文件.txt";
        tmp_repo
            .write_file(chinese_filename, "Hello, 世界!\n", true)
            .unwrap();

        // Trigger AI checkpoint
        tmp_repo
            .trigger_checkpoint_with_ai("mock_ai", None, None)
            .unwrap();

        // Commit
        let authorship_log = tmp_repo.commit_with_message("Add Chinese file").unwrap();

        // Debug output
        println!(
            "Authorship log attestations: {:?}",
            authorship_log.attestations
        );

        // The attestation should include the Chinese filename
        assert_eq!(
            authorship_log.attestations.len(),
            1,
            "Should have 1 attestation for the Chinese-named file"
        );
        assert_eq!(
            authorship_log.attestations[0].file_path, chinese_filename,
            "File path should be the UTF-8 filename"
        );
    }
}
