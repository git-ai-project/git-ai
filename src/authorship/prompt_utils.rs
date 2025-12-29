use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::transcript::AiTranscript;
use crate::commands::checkpoint_agent::agent_presets::{
    ClaudePreset, ContinueCliPreset, CursorPreset, GeminiPreset, GithubCopilotPreset,
};
use crate::error::GitAiError;
use crate::git::refs::{get_authorship, grep_ai_notes};
use crate::git::repository::Repository;
use crate::observability::log_error;
use crate::utils::debug_log;
use std::collections::HashMap;

/// Find a prompt in the repository history
///
/// If `commit` is provided, look only in that specific commit.
/// Otherwise, search through history and skip `offset` occurrences (0 = most recent).
pub fn find_prompt(
    repo: &Repository,
    prompt_id: &str,
    commit: Option<&str>,
    offset: usize,
) -> Result<(String, PromptRecord), GitAiError> {
    if let Some(commit_rev) = commit {
        // Look in specific commit
        find_prompt_in_commit(repo, prompt_id, commit_rev)
    } else {
        // Search through history with offset
        find_prompt_in_history(repo, prompt_id, offset)
    }
}

/// Find a prompt in a specific commit
pub fn find_prompt_in_commit(
    repo: &Repository,
    prompt_id: &str,
    commit_rev: &str,
) -> Result<(String, PromptRecord), GitAiError> {
    // Resolve the revision to a commit SHA
    let commit = repo.revparse_single(commit_rev)?;
    let commit_sha = commit.id();

    // Get the authorship log for this commit
    let authorship_log = get_authorship(repo, &commit_sha).ok_or_else(|| {
        GitAiError::Generic(format!(
            "No authorship data found for commit: {}",
            commit_rev
        ))
    })?;

    // Look for the prompt in the log
    authorship_log
        .metadata
        .prompts
        .get(prompt_id)
        .map(|prompt| (commit_sha, prompt.clone()))
        .ok_or_else(|| {
            GitAiError::Generic(format!(
                "Prompt '{}' not found in commit {}",
                prompt_id, commit_rev
            ))
        })
}

/// Find a prompt in history, skipping `offset` occurrences
/// Returns the (N+1)th occurrence where N = offset (0 = most recent)
pub fn find_prompt_in_history(
    repo: &Repository,
    prompt_id: &str,
    offset: usize,
) -> Result<(String, PromptRecord), GitAiError> {
    // Use git grep to search for the prompt ID in authorship notes
    // grep_ai_notes returns commits sorted by date (newest first)
    let shas = grep_ai_notes(repo, &format!("\"{}\"", prompt_id)).unwrap_or_default();

    if shas.is_empty() {
        return Err(GitAiError::Generic(format!(
            "Prompt not found in history: {}",
            prompt_id
        )));
    }

    // Iterate through commits, looking for the prompt and counting occurrences
    let mut found_count = 0;
    for sha in &shas {
        if let Some(authorship_log) = get_authorship(repo, sha) {
            if let Some(prompt) = authorship_log.metadata.prompts.get(prompt_id) {
                if found_count == offset {
                    return Ok((sha.clone(), prompt.clone()));
                }
                found_count += 1;
            }
        }
    }

    // If we get here, we didn't find enough occurrences
    if found_count == 0 {
        Err(GitAiError::Generic(format!(
            "Prompt not found in history: {}",
            prompt_id
        )))
    } else {
        Err(GitAiError::Generic(format!(
            "Prompt '{}' found {} time(s), but offset {} requested (max offset: {})",
            prompt_id,
            found_count,
            offset,
            found_count - 1
        )))
    }
}

/// Find a prompt, trying the database first, then falling back to repository if provided
///
/// Returns `(Option<commit_sha>, PromptRecord)` where commit_sha is None if found in DB
/// and Some(sha) if found in repository.
pub fn find_prompt_with_db_fallback(
    prompt_id: &str,
    repo: Option<&Repository>,
) -> Result<(Option<String>, PromptRecord), GitAiError> {
    // First, try to get from database
    let db = InternalDatabase::global()?;
    let db_guard = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    if let Some(db_record) = db_guard.get_prompt(prompt_id)? {
        // Convert PromptDbRecord to PromptRecord
        let prompt_record = db_record.to_prompt_record();
        return Ok((db_record.commit_sha, prompt_record));
    }

    // Not found in DB, try repository if provided
    if let Some(repo) = repo {
        // Try to find in history (most recent occurrence)
        match find_prompt_in_history(repo, prompt_id, 0) {
            Ok((commit_sha, prompt)) => Ok((Some(commit_sha), prompt)),
            Err(_) => Err(GitAiError::Generic(format!(
                "Prompt '{}' not found in database or repository",
                prompt_id
            ))),
        }
    } else {
        Err(GitAiError::Generic(format!(
            "Prompt '{}' not found in database and no repository provided",
            prompt_id
        )))
    }
}

/// Result of attempting to update a prompt from a tool
pub enum PromptUpdateResult {
    Updated(AiTranscript, String), // (new_transcript, new_model)
    Unchanged,                      // No update available or needed
    Failed(GitAiError),             // Error occurred but not fatal
}

/// Update a prompt by fetching latest transcript from the tool
///
/// This function NEVER panics or stops execution on errors.
/// Errors are logged but returned as PromptUpdateResult::Failed.
pub fn update_prompt_from_tool(
    tool: &str,
    external_thread_id: &str,
    agent_metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    match tool {
        "cursor" => update_cursor_prompt(external_thread_id),
        "claude" => update_claude_prompt(agent_metadata, current_model),
        "gemini" => update_gemini_prompt(agent_metadata, current_model),
        "github-copilot" => update_github_copilot_prompt(agent_metadata, current_model),
        "continue-cli" => update_continue_cli_prompt(agent_metadata, current_model),
        _ => {
            debug_log(&format!("Unknown tool: {}", tool));
            PromptUpdateResult::Unchanged
        }
    }
}

/// Update Cursor prompt by fetching from Cursor's database
fn update_cursor_prompt(conversation_id: &str) -> PromptUpdateResult {
    let res = CursorPreset::fetch_latest_cursor_conversation(conversation_id);
    match res {
        Ok(Some((latest_transcript, latest_model))) => {
            PromptUpdateResult::Updated(latest_transcript, latest_model)
        }
        Ok(None) => PromptUpdateResult::Unchanged,
        Err(e) => {
            debug_log(&format!(
                "Failed to fetch latest Cursor conversation for ID {}: {}",
                conversation_id, e
            ));
            log_error(
                &e,
                Some(serde_json::json!({
                    "agent_tool": "cursor",
                    "operation": "fetch_latest_cursor_conversation"
                })),
            );
            PromptUpdateResult::Failed(e)
        }
    }
}

/// Update Claude prompt from transcript file
fn update_claude_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Try to read and parse the transcript JSONL
            match ClaudePreset::transcript_and_model_from_claude_code_jsonl(transcript_path) {
                Ok((transcript, model)) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    PromptUpdateResult::Updated(
                        transcript,
                        model.unwrap_or_else(|| current_model.to_string()),
                    )
                }
                Err(e) => {
                    debug_log(&format!(
                        "Failed to parse Claude JSONL transcript from {}: {}",
                        transcript_path, e
                    ));
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "claude",
                            "operation": "transcript_and_model_from_claude_code_jsonl"
                        })),
                    );
                    PromptUpdateResult::Failed(e)
                }
            }
        } else {
            // No transcript_path in metadata
            PromptUpdateResult::Unchanged
        }
    } else {
        // No agent_metadata available
        PromptUpdateResult::Unchanged
    }
}

/// Update Gemini prompt from transcript file
fn update_gemini_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Try to read and parse the transcript JSON
            match GeminiPreset::transcript_and_model_from_gemini_json(transcript_path) {
                Ok((transcript, model)) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    PromptUpdateResult::Updated(
                        transcript,
                        model.unwrap_or_else(|| current_model.to_string()),
                    )
                }
                Err(e) => {
                    debug_log(&format!(
                        "Failed to parse Gemini JSON transcript from {}: {}",
                        transcript_path, e
                    ));
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "gemini",
                            "operation": "transcript_and_model_from_gemini_json"
                        })),
                    );
                    PromptUpdateResult::Failed(e)
                }
            }
        } else {
            // No transcript_path in metadata
            PromptUpdateResult::Unchanged
        }
    } else {
        // No agent_metadata available
        PromptUpdateResult::Unchanged
    }
}

/// Update GitHub Copilot prompt from chat session file
fn update_github_copilot_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(chat_session_path) = metadata.get("chat_session_path") {
            // Try to read and parse the chat session JSON
            match GithubCopilotPreset::transcript_and_model_from_copilot_session_json(
                chat_session_path,
            ) {
                Ok((transcript, model, _)) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    PromptUpdateResult::Updated(
                        transcript,
                        model.unwrap_or_else(|| current_model.to_string()),
                    )
                }
                Err(e) => {
                    debug_log(&format!(
                        "Failed to parse GitHub Copilot chat session JSON from {}: {}",
                        chat_session_path, e
                    ));
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "github-copilot",
                            "operation": "transcript_and_model_from_copilot_session_json"
                        })),
                    );
                    PromptUpdateResult::Failed(e)
                }
            }
        } else {
            // No chat_session_path in metadata
            PromptUpdateResult::Unchanged
        }
    } else {
        // No agent_metadata available
        PromptUpdateResult::Unchanged
    }
}

/// Update Continue CLI prompt from transcript file
fn update_continue_cli_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Try to read and parse the transcript JSON
            match ContinueCliPreset::transcript_from_continue_json(transcript_path) {
                Ok(transcript) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    // IMPORTANT: Always preserve the original model from agent_id (don't overwrite)
                    PromptUpdateResult::Updated(transcript, current_model.to_string())
                }
                Err(e) => {
                    debug_log(&format!(
                        "Failed to parse Continue CLI JSON transcript from {}: {}",
                        transcript_path, e
                    ));
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "continue-cli",
                            "operation": "transcript_from_continue_json"
                        })),
                    );
                    PromptUpdateResult::Failed(e)
                }
            }
        } else {
            // No transcript_path in metadata
            PromptUpdateResult::Unchanged
        }
    } else {
        // No agent_metadata available
        PromptUpdateResult::Unchanged
    }
}
