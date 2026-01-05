use crate::api::{ApiClient, ApiContext};
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::prompt_utils::{update_prompt_from_tool, PromptUpdateResult};
use crate::authorship::secrets::{redact_secrets_from_prompts, strip_prompt_messages};
use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::{Checkpoint, CheckpointKind};
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::refs::notes_add;
use crate::git::repository::Repository;
use crate::utils::debug_log;
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal;

pub fn post_commit(
    repo: &Repository,
    base_commit: Option<String>,
    commit_sha: String,
    human_author: String,
    supress_output: bool,
) -> Result<(String, AuthorshipLog), GitAiError> {
    // Use base_commit parameter if provided, otherwise use "initial" for empty repos
    // This matches the convention in checkpoint.rs
    let parent_sha = base_commit.unwrap_or_else(|| "initial".to_string());

    // Initialize the new storage system
    let repo_storage = &repo.storage;
    let working_log = repo_storage.working_log_for_base_commit(&parent_sha);

    // Pull all working log entries from the parent commit

    let mut parent_working_log = working_log.read_all_checkpoints()?;

    // debug_log(&format!(
    //     "edited files: {:?}",
    //     parent_working_log.edited_files
    // ));

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

    // Filter out untracked files from the working log
    let filtered_working_log =
        filter_untracked_files(repo, &parent_working_log, &commit_sha, None)?;

    // Create VirtualAttributions from working log (fast path - no blame)
    // We don't need to run blame because we only care about the working log data
    // that was accumulated since the parent commit
    let working_va = VirtualAttributions::from_just_working_log(
        repo.clone(),
        parent_sha.clone(),
        Some(human_author.clone()),
    )?;

    // Get pathspecs for files in the working log
    let pathspecs: HashSet<String> = filtered_working_log
        .iter()
        .flat_map(|cp| cp.entries.iter().map(|e| e.file.clone()))
        .collect();

    // Split VirtualAttributions into committed (authorship log) and uncommitted (INITIAL)
    let (mut authorship_log, initial_attributions) = working_va
        .to_authorship_log_and_initial_working_log(
            repo,
            &parent_sha,
            &commit_sha,
            Some(&pathspecs),
        )?;

    authorship_log.metadata.base_commit_sha = commit_sha.clone();

    // Handle prompts based on prompt_storage setting and exclusion rules
    let should_exclude = Config::get().should_exclude_prompts(&Some(repo.clone()));
    let prompt_storage = Config::get().prompt_storage();

    match prompt_storage {
        "local" => {
            // Local only: strip all messages from notes (they stay in sqlite only)
            strip_prompt_messages(&mut authorship_log.metadata.prompts);
        }
        "notes" => {
            // Store in notes: redact secrets but keep messages in notes
            if should_exclude {
                strip_prompt_messages(&mut authorship_log.metadata.prompts);
            } else {
                let count = redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
                if count > 0 {
                    debug_log(&format!("Redacted {} secrets from prompts", count));
                }
            }
        }
        _ => {
            // "default" - attempt CAS upload, NEVER keep messages in notes
            // Check conditions for CAS upload:
            // - prompt_storage == "default" (implied here)
            // - repo not in exclusion list
            // - user is logged in OR using custom API URL
            let context = ApiContext::new(None);
            let client = ApiClient::new(context);
            let using_custom_api =
                Config::get().api_base_url() != crate::config::DEFAULT_API_BASE_URL;
            let should_enqueue_cas =
                !should_exclude && (client.is_logged_in() || using_custom_api);

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

    // Serialize the authorship log
    let authorship_json = authorship_log
        .serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))?;

    notes_add(repo, &commit_sha, &authorship_json)?;

    // Write INITIAL file for uncommitted AI attributions (if any)
    if !initial_attributions.files.is_empty() {
        let new_working_log = repo_storage.working_log_for_base_commit(&commit_sha);
        new_working_log
            .write_initial_attributions(initial_attributions.files, initial_attributions.prompts)?;
    }

    // // Clean up old working log
    repo_storage.delete_working_log_for_base_commit(&parent_sha)?;

    if !supress_output {
        let stats = stats_for_commit_stats(repo, &commit_sha, &[])?;
        // Only print stats if we're in an interactive terminal
        let is_interactive = std::io::stdout().is_terminal();
        write_stats_to_terminal(&stats, is_interactive);
    }
    Ok((commit_sha.to_string(), authorship_log))
}

/// Filter out working log entries for untracked files
pub fn filter_untracked_files(
    repo: &Repository,
    working_log: &[Checkpoint],
    commit_sha: &str,
    pathspecs: Option<&HashSet<String>>,
) -> Result<Vec<Checkpoint>, GitAiError> {
    // Get all files changed in current commit in ONE git command (scoped to pathspecs)
    // If a file from the working log is in this set, it was committed. Otherwise, it was untracked.
    let committed_files = repo.list_commit_files(commit_sha, pathspecs)?;

    // Filter the working log to only include files that were actually committed
    let mut filtered_checkpoints = Vec::new();

    for checkpoint in working_log {
        let mut filtered_entries = Vec::new();

        for entry in &checkpoint.entries {
            // Keep entry only if this file was in the commit
            if committed_files.contains(&entry.file) {
                filtered_entries.push(entry.clone());
            }
        }

        // Only include checkpoints that have at least one committed file entry
        if !filtered_entries.is_empty() {
            let mut filtered_checkpoint = checkpoint.clone();
            filtered_checkpoint.entries = filtered_entries;
            filtered_checkpoints.push(filtered_checkpoint);
        }
    }

    Ok(filtered_checkpoints)
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
            agent_checkpoint_indices
                .entry(key)
                .or_insert_with(Vec::new)
                .push(idx);
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
    prompts: &mut std::collections::BTreeMap<String, crate::authorship::authorship_log::PromptRecord>,
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
            repo.remotes_with_urls()
                .ok()
                .and_then(|remotes| {
                    remotes
                        .into_iter()
                        .find(|(name, _)| name == &remote_name)
                        .map(|(_, url)| url)
                })
        });

    if let Some(url) = repo_url {
        metadata.insert("repo_url".to_string(), url);
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

#[cfg(test)]
mod tests {
    use crate::git::test_utils::TmpRepo;

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
}
