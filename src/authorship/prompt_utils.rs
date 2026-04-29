use crate::authorship::authorship_log::PromptRecord;
use crate::commands::checkpoint_agent::transcript_readers;
use crate::error::GitAiError;
use crate::git::refs::{get_authorship, grep_ai_notes};
use crate::git::repository::Repository;
use crate::observability::log_error;
use std::collections::HashMap;
use std::path::Path;

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

/// Find a prompt in a specific commit (searches both prompts and sessions)
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

    // Look for the prompt in the prompts map first
    if let Some(prompt) = authorship_log.metadata.prompts.get(prompt_id) {
        return Ok((commit_sha, prompt.clone()));
    }

    // Fall back to sessions map (session IDs start with "s_")
    // Strip ::t_ trace suffix if present — attestation hashes use s_xxx::t_yyy but session keys are just s_xxx
    let session_key = if prompt_id.starts_with("s_") {
        prompt_id.split("::").next().unwrap_or(prompt_id)
    } else {
        prompt_id
    };
    if let Some(session) = authorship_log.metadata.sessions.get(session_key) {
        return Ok((commit_sha, session.to_prompt_record()));
    }

    Err(GitAiError::Generic(format!(
        "Prompt '{}' not found in commit {}",
        prompt_id, commit_rev
    )))
}

/// Find a prompt in history, skipping `offset` occurrences
/// Returns the (N+1)th occurrence where N = offset (0 = most recent)
pub fn find_prompt_in_history(
    repo: &Repository,
    prompt_id: &str,
    offset: usize,
) -> Result<(String, PromptRecord), GitAiError> {
    // Strip ::t_ trace suffix for session lookups — attestation hashes use s_xxx::t_yyy
    // but session keys in metadata are just s_xxx
    let session_key = if prompt_id.starts_with("s_") {
        prompt_id.split("::").next().unwrap_or(prompt_id)
    } else {
        prompt_id
    };

    // Use git grep to search for the prompt ID in authorship notes
    // grep_ai_notes returns commits sorted by date (newest first)
    let shas = grep_ai_notes(repo, &format!("\"{}\"", session_key)).unwrap_or_default();

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
            // Check prompts map first
            if let Some(prompt) = authorship_log.metadata.prompts.get(prompt_id) {
                if found_count == offset {
                    return Ok((sha.clone(), prompt.clone()));
                }
                found_count += 1;
            // Then check sessions map
            } else if let Some(session) = authorship_log.metadata.sessions.get(session_key) {
                if found_count == offset {
                    return Ok((sha.clone(), session.to_prompt_record()));
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

/// Result of attempting to update a prompt from a tool
pub enum PromptUpdateResult {
    Updated(String),    // new_model
    Unchanged,          // No update available or needed
    Failed(GitAiError), // Error occurred but not fatal
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
        "cursor" => update_cursor_prompt(external_thread_id, agent_metadata, current_model),
        "claude" => update_claude_prompt(agent_metadata, current_model),
        "codex" => update_codex_prompt(agent_metadata, current_model),
        "gemini" => update_gemini_prompt(agent_metadata, current_model),
        "github-copilot" => update_github_copilot_prompt(agent_metadata, current_model),
        "continue-cli" => update_continue_cli_prompt(agent_metadata, current_model),
        "droid" => update_droid_prompt(agent_metadata, current_model),
        "amp" => update_amp_prompt(external_thread_id, agent_metadata, current_model),
        "opencode" => update_opencode_prompt(external_thread_id, agent_metadata, current_model),
        "pi" => update_pi_prompt(agent_metadata, current_model),
        "windsurf" => update_windsurf_prompt(agent_metadata, current_model),
        _ => {
            tracing::debug!("Unknown tool: {}", tool);
            PromptUpdateResult::Unchanged
        }
    }
}

/// Update Codex prompt from rollout transcript file
#[doc(hidden)]
pub fn update_codex_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            match transcript_readers::read_codex_jsonl(Path::new(transcript_path)) {
                Ok((_, model)) => {
                    PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to parse Codex rollout JSONL transcript from {}: {}",
                        transcript_path,
                        e
                    );
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "codex",
                            "operation": "transcript_and_model_from_codex_rollout_jsonl"
                        })),
                    );
                    PromptUpdateResult::Failed(e)
                }
            }
        } else {
            PromptUpdateResult::Unchanged
        }
    } else {
        PromptUpdateResult::Unchanged
    }
}

/// Update Cursor prompt by re-reading the JSONL transcript file
fn update_cursor_prompt(
    _conversation_id: &str,
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            match transcript_readers::read_cursor_jsonl(Path::new(transcript_path)) {
                Ok((_, _)) => PromptUpdateResult::Updated(current_model.to_string()),
                Err(e) => {
                    tracing::debug!(
                        "Failed to parse Cursor JSONL transcript from {}: {}",
                        transcript_path,
                        e
                    );
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "cursor",
                            "operation": "transcript_and_model_from_cursor_jsonl"
                        })),
                    );
                    PromptUpdateResult::Failed(e)
                }
            }
        } else {
            PromptUpdateResult::Unchanged
        }
    } else {
        PromptUpdateResult::Unchanged
    }
}

/// Update Claude prompt from transcript file
#[doc(hidden)]
pub fn update_claude_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Try to read and parse the transcript JSONL
            match transcript_readers::read_claude_jsonl(Path::new(transcript_path)) {
                Ok((_, model)) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to parse Claude JSONL transcript from {}: {}",
                        transcript_path,
                        e
                    );
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
#[doc(hidden)]
pub fn update_gemini_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Try to read and parse the transcript JSON
            match transcript_readers::read_gemini_json(Path::new(transcript_path)) {
                Ok((_, model)) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to parse Gemini JSON transcript from {}: {}",
                        transcript_path,
                        e
                    );
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
#[doc(hidden)]
pub fn update_github_copilot_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(chat_session_path) = metadata.get("chat_session_path") {
            // Try to read and parse the chat session JSON
            match transcript_readers::read_copilot_session_json(Path::new(chat_session_path)) {
                Ok((_, model, _)) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to parse GitHub Copilot chat session JSON from {}: {}",
                        chat_session_path,
                        e
                    );
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
#[doc(hidden)]
pub fn update_continue_cli_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Try to load transcript from agent_metadata if available
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Try to read and parse the transcript JSON
            match transcript_readers::read_continue_json(Path::new(transcript_path)) {
                Ok(_) => {
                    // Update to the latest transcript (similar to Cursor behavior)
                    // This handles both cases: initial load failure and getting latest version
                    // IMPORTANT: Always preserve the original model from agent_id (don't overwrite)
                    PromptUpdateResult::Updated(current_model.to_string())
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to parse Continue CLI JSON transcript from {}: {}",
                        transcript_path,
                        e
                    );
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

/// Update Droid prompt from transcript and settings files
#[doc(hidden)]
pub fn update_droid_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Validate transcript can be parsed
            if let Err(e) = transcript_readers::read_droid_jsonl(Path::new(transcript_path)) {
                tracing::debug!(
                    "Failed to parse Droid JSONL transcript from {}: {}",
                    transcript_path,
                    e
                );
                log_error(
                    &e,
                    Some(serde_json::json!({
                        "agent_tool": "droid",
                        "operation": "transcript_and_model_from_droid_jsonl"
                    })),
                );
                return PromptUpdateResult::Failed(e);
            }

            // Re-parse model from settings.json
            let model = if let Some(settings_path) = metadata.get("settings_path") {
                match transcript_readers::read_droid_model_from_settings(Path::new(settings_path)) {
                    Ok(Some(m)) => m,
                    Ok(None) => current_model.to_string(),
                    Err(e) => {
                        tracing::debug!(
                            "Failed to parse Droid settings.json from {}: {}",
                            settings_path,
                            e
                        );
                        current_model.to_string()
                    }
                }
            } else {
                current_model.to_string()
            };

            PromptUpdateResult::Updated(model)
        } else {
            // No transcript_path in metadata
            PromptUpdateResult::Unchanged
        }
    } else {
        // No agent_metadata available
        PromptUpdateResult::Unchanged
    }
}

/// Update Amp prompt by re-parsing the thread JSON file.
fn update_amp_prompt(
    thread_id: &str,
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    let result = if let Some(transcript_path) = metadata
        .and_then(|m| m.get("transcript_path"))
        .filter(|p| !p.trim().is_empty())
    {
        transcript_readers::read_amp_thread_json(Path::new(transcript_path))
            .map(|(transcript, model, _)| (transcript, model))
    } else if let Some(threads_dir) = metadata
        .and_then(|m| m.get("__test_amp_threads_path"))
        .filter(|p| !p.trim().is_empty())
    {
        let threads_dir = Path::new(threads_dir);
        if !thread_id.trim().is_empty() {
            transcript_readers::read_amp_thread_by_id_in_dir(threads_dir, thread_id)
        } else if let Some(tool_use_id) = metadata
            .and_then(|m| m.get("tool_use_id"))
            .filter(|p| !p.trim().is_empty())
        {
            transcript_readers::read_amp_thread_by_tool_use_id_in_dir(threads_dir, tool_use_id)
        } else {
            return PromptUpdateResult::Unchanged;
        }
    } else if !thread_id.trim().is_empty() {
        transcript_readers::read_amp_thread_by_id(thread_id)
    } else if let Some(tool_use_id) = metadata
        .and_then(|m| m.get("tool_use_id"))
        .filter(|p| !p.trim().is_empty())
    {
        let default_threads = match transcript_readers::amp_threads_path() {
            Ok(path) => path,
            Err(e) => return PromptUpdateResult::Failed(e),
        };
        transcript_readers::read_amp_thread_by_tool_use_id_in_dir(&default_threads, tool_use_id)
    } else {
        return PromptUpdateResult::Unchanged;
    };

    match result {
        Ok((_, model)) => {
            PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
        }
        Err(e) => {
            tracing::debug!(
                "Failed to fetch Amp transcript for thread {}: {}",
                thread_id,
                e
            );
            log_error(
                &e,
                Some(serde_json::json!({
                    "agent_tool": "amp",
                    "operation": "transcript_and_model_from_thread_path"
                })),
            );
            PromptUpdateResult::Failed(e)
        }
    }
}

/// Update OpenCode prompt by fetching latest transcript from storage
fn update_opencode_prompt(
    session_id: &str,
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // Check for test storage path override in metadata or env var
    let storage_path = if let Ok(env_path) = std::env::var("GIT_AI_OPENCODE_STORAGE_PATH") {
        Some(std::path::PathBuf::from(env_path))
    } else {
        metadata
            .and_then(|m| m.get("__test_storage_path"))
            .map(std::path::PathBuf::from)
    };

    let result = if let Some(path) = storage_path {
        transcript_readers::read_opencode_from_storage(&path, session_id)
    } else {
        transcript_readers::read_opencode_from_session(session_id)
    };

    match result {
        Ok((_, model)) => {
            PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
        }
        Err(e) => {
            tracing::debug!(
                "Failed to fetch OpenCode transcript for session {}: {}",
                session_id,
                e
            );
            log_error(
                &e,
                Some(serde_json::json!({
                    "agent_tool": "opencode",
                    "operation": "transcript_and_model_from_storage"
                })),
            );
            PromptUpdateResult::Failed(e)
        }
    }
}

/// Update Pi prompt from session JSONL file
fn update_pi_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    if let Some(session_path) = metadata
        .and_then(|m| m.get("session_path"))
        .filter(|path| !path.trim().is_empty())
    {
        match transcript_readers::read_pi_session(session_path) {
            Ok((_, model)) => {
                PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
            }
            Err(e) => {
                tracing::debug!(
                    "Failed to parse Pi session JSONL from {}: {}",
                    session_path,
                    e
                );
                log_error(
                    &e,
                    Some(serde_json::json!({
                        "agent_tool": "pi",
                        "operation": "transcript_and_model_from_pi_session"
                    })),
                );
                PromptUpdateResult::Failed(e)
            }
        }
    } else {
        PromptUpdateResult::Unchanged
    }
}

/// Update Windsurf prompt from transcript JSONL file
#[doc(hidden)]
pub fn update_windsurf_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            match transcript_readers::read_windsurf_jsonl(Path::new(transcript_path)) {
                Ok((_, model)) => {
                    PromptUpdateResult::Updated(model.unwrap_or_else(|| current_model.to_string()))
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to parse Windsurf JSONL transcript from {}: {}",
                        transcript_path,
                        e
                    );
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "windsurf",
                            "operation": "transcript_and_model_from_windsurf_jsonl"
                        })),
                    );
                    PromptUpdateResult::Failed(e)
                }
            }
        } else {
            PromptUpdateResult::Unchanged
        }
    } else {
        PromptUpdateResult::Unchanged
    }
}

/// Format a PromptRecord's messages into a human-readable transcript.
///
/// Filters out ToolUse messages; keeps User, Assistant, Thinking, and Plan.
/// Each message is prefixed with its role label.
pub fn format_transcript(_prompt: &PromptRecord) -> String {
    // PromptRecord no longer contains messages
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_update_prompt_from_tool_unknown() {
        let result = update_prompt_from_tool("unknown-tool", "thread-123", None, "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_codex_prompt_no_metadata() {
        let result = update_codex_prompt(None, "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_codex_prompt_no_transcript_path() {
        let metadata = HashMap::new();
        let result = update_codex_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_claude_prompt_no_metadata() {
        let result = update_claude_prompt(None, "claude-3");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_claude_prompt_no_transcript_path() {
        let metadata = HashMap::new();
        let result = update_claude_prompt(Some(&metadata), "claude-3");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_gemini_prompt_no_metadata() {
        let result = update_gemini_prompt(None, "gemini-pro");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_gemini_prompt_no_transcript_path() {
        let metadata = HashMap::new();
        let result = update_gemini_prompt(Some(&metadata), "gemini-pro");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_github_copilot_prompt_no_metadata() {
        let result = update_github_copilot_prompt(None, "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_github_copilot_prompt_no_session_path() {
        let metadata = HashMap::new();
        let result = update_github_copilot_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_continue_cli_prompt_no_metadata() {
        let result = update_continue_cli_prompt(None, "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_continue_cli_prompt_no_transcript_path() {
        let metadata = HashMap::new();
        let result = update_continue_cli_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_droid_prompt_no_metadata() {
        let result = update_droid_prompt(None, "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_droid_prompt_no_transcript_path() {
        let metadata = HashMap::new();
        let result = update_droid_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_prompt_from_tool_dispatch() {
        // Test that unknown tools return Unchanged
        let result = update_prompt_from_tool("unknown", "thread-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));

        // Test dispatch to cursor (may return Failed if cursor DB doesn't exist, which is expected)
        let result = update_prompt_from_tool("cursor", "thread-123", None, "model");
        assert!(matches!(
            result,
            PromptUpdateResult::Unchanged | PromptUpdateResult::Failed(_)
        ));

        // Test dispatch to claude
        let result = update_prompt_from_tool("claude", "thread-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));

        // Test dispatch to codex
        let result = update_prompt_from_tool("codex", "thread-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));

        // Test dispatch to gemini
        let result = update_prompt_from_tool("gemini", "thread-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));

        // Test dispatch to github-copilot
        let result = update_prompt_from_tool("github-copilot", "thread-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));

        // Test dispatch to continue-cli
        let result = update_prompt_from_tool("continue-cli", "thread-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));

        // Test dispatch to droid
        let result = update_prompt_from_tool("droid", "thread-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));

        // Test dispatch to amp (without metadata, returns Unchanged or Failed depending on local state)
        let result = update_prompt_from_tool("amp", "thread-123", None, "model");
        assert!(matches!(
            result,
            PromptUpdateResult::Unchanged | PromptUpdateResult::Failed(_)
        ));

        // Test dispatch to opencode (behavior depends on whether default storage exists)
        let result = update_prompt_from_tool("opencode", "session-123", None, "model");
        // Can be Unchanged, Failed, or Updated depending on storage availability
        match result {
            PromptUpdateResult::Unchanged
            | PromptUpdateResult::Failed(_)
            | PromptUpdateResult::Updated(_) => {}
        }

        // Test dispatch to windsurf
        let result = update_prompt_from_tool("windsurf", "trajectory-123", None, "model");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_codex_prompt_invalid_path() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "transcript_path".to_string(),
            "/nonexistent/path.jsonl".to_string(),
        );

        let result = update_codex_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Failed(_)));
    }

    #[test]
    fn test_update_claude_prompt_invalid_path() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "transcript_path".to_string(),
            "/nonexistent/path.jsonl".to_string(),
        );

        let result = update_claude_prompt(Some(&metadata), "claude-3");
        assert!(matches!(result, PromptUpdateResult::Failed(_)));
    }

    #[test]
    fn test_update_gemini_prompt_invalid_path() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "transcript_path".to_string(),
            "/nonexistent/path.json".to_string(),
        );

        let result = update_gemini_prompt(Some(&metadata), "gemini-pro");
        assert!(matches!(result, PromptUpdateResult::Failed(_)));
    }

    #[test]
    fn test_update_github_copilot_prompt_invalid_path() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "chat_session_path".to_string(),
            "/nonexistent/path.json".to_string(),
        );

        let result = update_github_copilot_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Failed(_)));
    }

    #[test]
    fn test_update_continue_cli_prompt_invalid_path() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "transcript_path".to_string(),
            "/nonexistent/path.json".to_string(),
        );

        let result = update_continue_cli_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Failed(_)));
    }

    #[test]
    fn test_update_droid_prompt_invalid_transcript_path() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "transcript_path".to_string(),
            "/nonexistent/path.jsonl".to_string(),
        );

        let result = update_droid_prompt(Some(&metadata), "gpt-4");
        assert!(matches!(result, PromptUpdateResult::Failed(_)));
    }

    #[test]
    fn test_update_windsurf_prompt_no_metadata() {
        let result = update_windsurf_prompt(None, "unknown");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_windsurf_prompt_no_transcript_path() {
        let metadata = HashMap::new();
        let result = update_windsurf_prompt(Some(&metadata), "unknown");
        assert!(matches!(result, PromptUpdateResult::Unchanged));
    }

    #[test]
    fn test_update_windsurf_prompt_invalid_path() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "transcript_path".to_string(),
            "/nonexistent/path.jsonl".to_string(),
        );

        let result = update_windsurf_prompt(Some(&metadata), "unknown");
        assert!(matches!(result, PromptUpdateResult::Failed(_)));
    }
}
