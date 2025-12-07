use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::stats::{stats_for_commit_stats, write_stats_to_terminal};
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::authorship::working_log::Checkpoint;
use crate::commands::checkpoint_agent::agent_presets::{ClaudePreset, CursorPreset, GeminiPreset, GithubCopilotPreset};
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::refs::notes_add;
use crate::git::repository::Repository;
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

    // Strip prompt messages if ignore_prompts is enabled
    if Config::get().ignore_prompts() {
        strip_prompt_messages(&mut authorship_log.metadata.prompts);
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
    // if !cfg!(debug_assertions) {
    repo_storage.delete_working_log_for_base_commit(&parent_sha)?;
    // }

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
            // Dispatch to tool-specific update logic
            let updated_data = match agent_id.tool.as_str() {
                "cursor" => {
                    let res = CursorPreset::fetch_latest_cursor_conversation(&agent_id.id);
                    match res {
                        Ok(Some((latest_transcript, latest_model))) => {
                            Some((latest_transcript, latest_model))
                        }
                        Ok(None) => None,
                        Err(_e) => {
                            // TODO Log error to sentry
                            None
                        }
                    }
                }
                "github-copilot" => {
                    // Try to load transcript from agent_metadata if available
                    if let Some(metadata) = &checkpoint.agent_metadata {
                        if let Some(chat_session_path) = metadata.get("chat_session_path") {
                            // Try to read and parse the chat session JSON
                            match GithubCopilotPreset::transcript_and_model_from_copilot_session_json(chat_session_path) {
                                Ok((transcript, model, _)) => {
                                    // Update to the latest transcript (similar to Cursor behavior)
                                    // This handles both cases: initial load failure and getting latest version
                                    Some((transcript, model.unwrap_or_else(|| agent_id.model.clone())))
                                }
                                Err(_e) => {
                                    // TODO Log error to sentry
                                    None
                                }
                            }
                        } else {
                            // No chat_session_path in metadata
                            None
                        }
                    } else {
                        // No agent_metadata available
                        None
                    }
                }
                "claude" => {
                    // Try to load transcript from agent_metadata if available
                    if let Some(metadata) = &checkpoint.agent_metadata {
                        if let Some(transcript_path) = metadata.get("transcript_path") {
                            // Try to read and parse the transcript JSONL
                            match ClaudePreset::transcript_and_model_from_claude_code_jsonl(transcript_path) {
                                Ok((transcript, model)) => {
                                    // Update to the latest transcript (similar to Cursor behavior)
                                    // This handles both cases: initial load failure and getting latest version
                                    Some((transcript, model.unwrap_or_else(|| agent_id.model.clone())))
                                }
                                Err(_e) => {
                                    // TODO Log error to sentry
                                    None
                                }
                            }
                        } else {
                            // No transcript_path in metadata
                            None
                        }
                    } else {
                        // No agent_metadata available
                        None
                    }
                }
                "gemini" => {
                    // Try to load transcript from agent_metadata if available
                    if let Some(metadata) = &checkpoint.agent_metadata {
                        if let Some(transcript_path) = metadata.get("transcript_path") {
                            // Try to read and parse the transcript JSON
                            match GeminiPreset::transcript_and_model_from_gemini_json(transcript_path) {
                                Ok((transcript, model)) => {
                                    // Update to the latest transcript (similar to Cursor behavior)
                                    // This handles both cases: initial load failure and getting latest version
                                    Some((transcript, model.unwrap_or_else(|| agent_id.model.clone())))
                                }
                                Err(_e) => {
                                    // TODO Log error to sentry
                                    None
                                }
                            }
                        } else {
                            // No transcript_path in metadata
                            None
                        }
                    } else {
                        // No agent_metadata available
                        None
                    }
                }
                _ => {
                    // Unknown tool, skip updating
                    None
                }
            };

            // Apply the update to the last checkpoint only
            if let Some((latest_transcript, latest_model)) = updated_data {
                let checkpoint = &mut checkpoints[last_idx];
                checkpoint.transcript = Some(latest_transcript);
                if let Some(agent_id) = &mut checkpoint.agent_id {
                    agent_id.model = latest_model;
                }
            }
        }
    }

    Ok(())
}

/// Strip messages from prompts if ignore_prompts config is enabled
/// This is called only in post_commit when writing prompts to git history
fn strip_prompt_messages(prompts: &mut std::collections::BTreeMap<String, PromptRecord>) {
    for record in prompts.values_mut() {
        record.messages.clear();
    }
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
