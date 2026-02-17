use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::transcript::AiTranscript;
use crate::commands::checkpoint_agent::agent_presets::{
    ClaudePreset, CodexPreset, ContinueCliPreset, CursorPreset, DroidPreset, GeminiPreset,
    GithubCopilotPreset,
};
use crate::commands::checkpoint_agent::opencode_preset::OpenCodePreset;
use crate::error::GitAiError;
use crate::git::refs::{get_authorship, grep_ai_notes};
use crate::git::repository::Repository;
use crate::observability::log_error;
use crate::utils::debug_log;
use std::collections::{HashMap, HashSet};

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
        if let Some(authorship_log) = get_authorship(repo, sha)
            && let Some(prompt) = authorship_log.metadata.prompts.get(prompt_id)
        {
            if found_count == offset {
                return Ok((sha.clone(), prompt.clone()));
            }
            found_count += 1;
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
    Unchanged,                     // No update available or needed
    Failed(GitAiError),            // Error occurred but not fatal
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
        "opencode" => update_opencode_prompt(external_thread_id, agent_metadata, current_model),
        _ => {
            debug_log(&format!("Unknown tool: {}", tool));
            PromptUpdateResult::Unchanged
        }
    }
}

/// Update Codex prompt from rollout transcript file
fn update_codex_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            match CodexPreset::transcript_and_model_from_codex_rollout_jsonl(transcript_path) {
                Ok((transcript, model)) => PromptUpdateResult::Updated(
                    transcript,
                    model.unwrap_or_else(|| current_model.to_string()),
                ),
                Err(e) => {
                    debug_log(&format!(
                        "Failed to parse Codex rollout JSONL transcript from {}: {}",
                        transcript_path, e
                    ));
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

/// Update Cursor prompt by fetching from Cursor's database
fn update_cursor_prompt(
    conversation_id: &str,
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    // For Cursor, we check the env var first (it represents the current database state),
    // then fall back to metadata (stored during checkpoint for git hook subprocesses
    // which don't inherit env vars).
    let res = if let Ok(env_db_path) = std::env::var("GIT_AI_CURSOR_GLOBAL_DB_PATH") {
        // Environment variable takes precedence (allows resync to use updated database)
        CursorPreset::fetch_cursor_conversation_from_db(
            std::path::Path::new(&env_db_path),
            conversation_id,
        )
    } else if let Some(db_path) = metadata.and_then(|m| m.get("__test_cursor_db_path")) {
        // Fall back to metadata path (for git hook subprocesses in tests)
        CursorPreset::fetch_cursor_conversation_from_db(
            std::path::Path::new(db_path),
            conversation_id,
        )
    } else {
        // Use default Cursor database location
        CursorPreset::fetch_latest_cursor_conversation(conversation_id)
    };
    match res {
        Ok(Some((latest_transcript, _db_model))) => {
            // For Cursor, preserve the model from the checkpoint (which came from hook input)
            // rather than using the database model
            PromptUpdateResult::Updated(latest_transcript, current_model.to_string())
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

/// Update Droid prompt from transcript and settings files
fn update_droid_prompt(
    metadata: Option<&HashMap<String, String>>,
    current_model: &str,
) -> PromptUpdateResult {
    if let Some(metadata) = metadata {
        if let Some(transcript_path) = metadata.get("transcript_path") {
            // Re-parse transcript
            let transcript =
                match DroidPreset::transcript_and_model_from_droid_jsonl(transcript_path) {
                    Ok((transcript, _model)) => transcript,
                    Err(e) => {
                        debug_log(&format!(
                            "Failed to parse Droid JSONL transcript from {}: {}",
                            transcript_path, e
                        ));
                        log_error(
                            &e,
                            Some(serde_json::json!({
                                "agent_tool": "droid",
                                "operation": "transcript_and_model_from_droid_jsonl"
                            })),
                        );
                        return PromptUpdateResult::Failed(e);
                    }
                };

            // Re-parse model from settings.json
            let model = if let Some(settings_path) = metadata.get("settings_path") {
                match DroidPreset::model_from_droid_settings_json(settings_path) {
                    Ok(Some(m)) => m,
                    Ok(None) => current_model.to_string(),
                    Err(e) => {
                        debug_log(&format!(
                            "Failed to parse Droid settings.json from {}: {}",
                            settings_path, e
                        ));
                        current_model.to_string()
                    }
                }
            } else {
                current_model.to_string()
            };

            PromptUpdateResult::Updated(transcript, model)
        } else {
            // No transcript_path in metadata
            PromptUpdateResult::Unchanged
        }
    } else {
        // No agent_metadata available
        PromptUpdateResult::Unchanged
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
        OpenCodePreset::transcript_and_model_from_storage(&path, session_id)
    } else {
        OpenCodePreset::transcript_and_model_from_session(session_id)
    };

    match result {
        Ok((transcript, model)) => PromptUpdateResult::Updated(
            transcript,
            model.unwrap_or_else(|| current_model.to_string()),
        ),
        Err(e) => {
            debug_log(&format!(
                "Failed to fetch OpenCode transcript for session {}: {}",
                session_id, e
            ));
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

/// Enrich prompts that have empty messages by falling back to the InternalDatabase (SQLite).
///
/// For each prompt in `prompts` whose ID is in `referenced_ids` and whose `messages` field
/// is empty, attempts to load the messages from the database.
pub fn enrich_prompt_messages(
    prompts: &mut HashMap<String, PromptRecord>,
    referenced_ids: &HashSet<&String>,
) {
    let ids_needing_messages: Vec<String> = prompts
        .iter()
        .filter(|(k, prompt)| referenced_ids.contains(k) && prompt.messages.is_empty())
        .map(|(id, _)| id.clone())
        .collect();

    if !ids_needing_messages.is_empty()
        && let Ok(db) = InternalDatabase::global()
        && let Ok(db_guard) = db.lock()
    {
        for id in &ids_needing_messages {
            if let Ok(Some(db_record)) = db_guard.get_prompt(id)
                && !db_record.messages.messages.is_empty()
                && let Some(prompt) = prompts.get_mut(id)
            {
                prompt.messages = db_record.messages.messages;
            }
        }
    }
}

/// Format a PromptRecord's messages into a human-readable transcript.
///
/// Filters out ToolUse messages; keeps User, Assistant, Thinking, and Plan.
/// Each message is prefixed with its role label.
pub fn format_transcript(prompt: &PromptRecord) -> String {
    use crate::authorship::transcript::Message;

    let mut output = String::new();
    for message in &prompt.messages {
        match message {
            Message::User { text, .. } => {
                output.push_str("User: ");
                output.push_str(text);
                output.push('\n');
            }
            Message::Assistant { text, .. } => {
                output.push_str("Assistant: ");
                output.push_str(text);
                output.push('\n');
            }
            Message::Thinking { text, .. } => {
                output.push_str("Thinking: ");
                output.push_str(text);
                output.push('\n');
            }
            Message::Plan { text, .. } => {
                output.push_str("Plan: ");
                output.push_str(text);
                output.push('\n');
            }
            Message::ToolUse { .. } => {
                // Skip tool use messages in formatted transcript
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::transcript::Message;
    use crate::authorship::working_log::AgentId;
    use crate::git::test_utils::TmpRepo;
    use std::collections::HashMap;

    // Helper function to create a test PromptRecord
    fn create_test_prompt_record(tool: &str, id: &str, model: &str) -> PromptRecord {
        PromptRecord {
            agent_id: AgentId {
                tool: tool.to_string(),
                id: id.to_string(),
                model: model.to_string(),
            },
            human_author: Some("test_user".to_string()),
            messages: vec![
                Message::User {
                    text: "Hello".to_string(),
                    timestamp: None,
                },
                Message::Assistant {
                    text: "Hi there".to_string(),
                    timestamp: None,
                },
            ],
            total_additions: 10,
            total_deletions: 5,
            accepted_lines: 8,
            overriden_lines: 2,
            messages_url: None,
        }
    }

    #[test]
    fn test_format_transcript_basic() {
        let prompt = create_test_prompt_record("test", "123", "gpt-4");
        let formatted = format_transcript(&prompt);

        assert!(formatted.contains("User: Hello\n"));
        assert!(formatted.contains("Assistant: Hi there\n"));
    }

    #[test]
    fn test_format_transcript_all_message_types() {
        let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
        prompt.messages = vec![
            Message::User {
                text: "User message".to_string(),
                timestamp: None,
            },
            Message::Assistant {
                text: "Assistant message".to_string(),
                timestamp: None,
            },
            Message::Thinking {
                text: "Thinking message".to_string(),
                timestamp: None,
            },
            Message::Plan {
                text: "Plan message".to_string(),
                timestamp: None,
            },
            Message::ToolUse {
                name: "test_tool".to_string(),
                input: serde_json::json!({"param": "value"}),
                timestamp: None,
            },
        ];

        let formatted = format_transcript(&prompt);

        assert!(formatted.contains("User: User message\n"));
        assert!(formatted.contains("Assistant: Assistant message\n"));
        assert!(formatted.contains("Thinking: Thinking message\n"));
        assert!(formatted.contains("Plan: Plan message\n"));
        // ToolUse should be filtered out
        assert!(!formatted.contains("test_tool"));
        assert!(!formatted.contains("ToolUse"));
    }

    #[test]
    fn test_format_transcript_empty() {
        let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
        prompt.messages = vec![];

        let formatted = format_transcript(&prompt);
        assert_eq!(formatted, "");
    }

    #[test]
    fn test_format_transcript_multiline() {
        let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
        prompt.messages = vec![Message::User {
            text: "Line 1\nLine 2\nLine 3".to_string(),
            timestamp: None,
        }];

        let formatted = format_transcript(&prompt);
        assert_eq!(formatted, "User: Line 1\nLine 2\nLine 3\n");
    }

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
    fn test_find_prompt_in_commit_integration() {
        // Create a test repository
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        // Create initial commit
        tmp_repo
            .write_file("test.txt", "initial content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("test_tool"))
            .expect("Failed to trigger checkpoint");

        let authorship = tmp_repo
            .commit_with_message("Initial commit")
            .expect("Failed to commit");

        // Get the prompt ID from the authorship log
        let prompt_id = authorship
            .metadata
            .prompts
            .keys()
            .next()
            .expect("No prompt found")
            .clone();

        // Get HEAD commit SHA
        let head_oid = tmp_repo.gitai_repo().head().unwrap().target().unwrap();
        let head_sha = head_oid.to_string();

        // Test finding the prompt
        let result = find_prompt_in_commit(tmp_repo.gitai_repo(), &prompt_id, "HEAD");
        assert!(result.is_ok());

        let (commit_sha, prompt) = result.unwrap();
        assert_eq!(commit_sha, head_sha);
        assert_eq!(prompt.agent_id.tool, "test_tool");
        assert_eq!(prompt.agent_id.id, "ai_agent");
        assert_eq!(prompt.agent_id.model, "gpt-4");
    }

    #[test]
    fn test_find_prompt_in_commit_not_found() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        // Create commit without AI checkpoint
        tmp_repo
            .write_file("test.txt", "initial content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_author("human_user")
            .expect("Failed to trigger checkpoint");
        tmp_repo
            .commit_with_message("Initial commit")
            .expect("Failed to commit");

        // Try to find a non-existent prompt
        // Human checkpoints have authorship data but no prompts
        let result = find_prompt_in_commit(tmp_repo.gitai_repo(), "nonexistent-prompt", "HEAD");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // Should get "Prompt not found" error since authorship exists but prompt doesn't
        assert!(
            err_msg.contains("Prompt") && err_msg.contains("not found"),
            "Unexpected error: {}",
            err_msg
        );
    }

    #[test]
    fn test_find_prompt_in_commit_invalid_revision() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        tmp_repo
            .write_file("test.txt", "initial content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_author("test_user")
            .expect("Failed to trigger checkpoint");
        tmp_repo
            .commit_with_message("Initial commit")
            .expect("Failed to commit");

        let result = find_prompt_in_commit(tmp_repo.gitai_repo(), "any-prompt", "invalid-revision");
        assert!(result.is_err());
    }

    #[test]
    fn test_find_prompt_in_history_basic() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        // Create first commit with AI checkpoint
        tmp_repo
            .write_file("test.txt", "v1\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("test_tool"))
            .expect("Failed to trigger checkpoint");
        let authorship1 = tmp_repo
            .commit_with_message("First commit")
            .expect("Failed to commit");

        let prompt_id = authorship1
            .metadata
            .prompts
            .keys()
            .next()
            .expect("No prompt found")
            .clone();

        // Test finding the prompt with offset 0 (most recent)
        let result = find_prompt_in_history(tmp_repo.gitai_repo(), &prompt_id, 0);
        assert!(result.is_ok());

        let (_sha, prompt) = result.unwrap();
        assert_eq!(prompt.agent_id.tool, "test_tool");
        assert_eq!(prompt.agent_id.id, "ai_agent");
    }

    #[test]
    fn test_find_prompt_in_history_with_offset() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        // Create first commit
        tmp_repo
            .write_file("test.txt", "v1\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_ai("Claude", Some("model-v1"), Some("test_tool"))
            .expect("Failed to trigger checkpoint");
        tmp_repo
            .commit_with_message("Commit 1")
            .expect("Failed to commit");

        // Get prompt ID from first commit
        let head_oid = tmp_repo.gitai_repo().head().unwrap().target().unwrap();
        let head_sha = head_oid.to_string();
        let authorship = get_authorship(tmp_repo.gitai_repo(), &head_sha).unwrap();
        let prompt_id = authorship
            .metadata
            .prompts
            .keys()
            .next()
            .expect("No prompt found")
            .clone();

        // At this point, offset 0 should work, offset 1 should fail
        let result = find_prompt_in_history(tmp_repo.gitai_repo(), &prompt_id, 0);
        assert!(result.is_ok());

        let result = find_prompt_in_history(tmp_repo.gitai_repo(), &prompt_id, 1);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("found 1 time(s), but offset 1 requested")
        );
    }

    #[test]
    fn test_find_prompt_in_history_not_found() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_author("human_user")
            .expect("Failed to trigger checkpoint");
        tmp_repo
            .commit_with_message("Commit")
            .expect("Failed to commit");

        let result = find_prompt_in_history(tmp_repo.gitai_repo(), "nonexistent-prompt", 0);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Prompt not found in history")
        );
    }

    #[test]
    fn test_find_prompt_delegates_to_commit() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("test_tool"))
            .expect("Failed to trigger checkpoint");
        let authorship = tmp_repo
            .commit_with_message("Test commit")
            .expect("Failed to commit");

        let prompt_id = authorship
            .metadata
            .prompts
            .keys()
            .next()
            .expect("No prompt found")
            .clone();

        // Test with commit specified
        let result = find_prompt(tmp_repo.gitai_repo(), &prompt_id, Some("HEAD"), 0);
        assert!(result.is_ok());
        let (_sha, prompt) = result.unwrap();
        assert_eq!(prompt.agent_id.tool, "test_tool");
        assert_eq!(prompt.agent_id.id, "ai_agent");
    }

    #[test]
    fn test_find_prompt_delegates_to_history() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("test_tool"))
            .expect("Failed to trigger checkpoint");
        let authorship = tmp_repo
            .commit_with_message("Test commit")
            .expect("Failed to commit");

        let prompt_id = authorship
            .metadata
            .prompts
            .keys()
            .next()
            .expect("No prompt found")
            .clone();

        // Test without commit (searches history)
        let result = find_prompt(tmp_repo.gitai_repo(), &prompt_id, None, 0);
        assert!(result.is_ok());
        let (_sha, prompt) = result.unwrap();
        assert_eq!(prompt.agent_id.tool, "test_tool");
        assert_eq!(prompt.agent_id.id, "ai_agent");
    }

    #[test]
    fn test_find_prompt_with_db_fallback_no_db_no_repo() {
        // Test when prompt is not in DB and no repo is provided
        let result = find_prompt_with_db_fallback("nonexistent-prompt", None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not found in database and no repository provided")
        );
    }

    #[test]
    fn test_find_prompt_with_db_fallback_no_db_with_repo() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("test_tool"))
            .expect("Failed to trigger checkpoint");
        let authorship = tmp_repo
            .commit_with_message("Test commit")
            .expect("Failed to commit");

        let prompt_id = authorship
            .metadata
            .prompts
            .keys()
            .next()
            .expect("No prompt found")
            .clone();

        // Test fallback to repository
        let result = find_prompt_with_db_fallback(&prompt_id, Some(tmp_repo.gitai_repo()));
        assert!(result.is_ok());
        let (commit_sha, prompt) = result.unwrap();
        assert!(commit_sha.is_some());
        assert_eq!(prompt.agent_id.tool, "test_tool");
    }

    #[test]
    fn test_find_prompt_with_db_fallback_not_in_repo() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_author("human_user")
            .expect("Failed to trigger checkpoint");
        tmp_repo
            .commit_with_message("Test commit")
            .expect("Failed to commit");

        let result =
            find_prompt_with_db_fallback("nonexistent-prompt", Some(tmp_repo.gitai_repo()));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not found in database or repository")
        );
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

        // Test dispatch to opencode (behavior depends on whether default storage exists)
        let result = update_prompt_from_tool("opencode", "session-123", None, "model");
        // Can be Unchanged, Failed, or Updated depending on storage availability
        match result {
            PromptUpdateResult::Unchanged
            | PromptUpdateResult::Failed(_)
            | PromptUpdateResult::Updated(_, _) => {}
        }
    }

    #[test]
    fn test_format_transcript_with_timestamps() {
        let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
        prompt.messages = vec![
            Message::User {
                text: "Question".to_string(),
                timestamp: Some("2024-01-01T12:00:00Z".to_string()),
            },
            Message::Assistant {
                text: "Answer".to_string(),
                timestamp: Some("2024-01-01T12:00:01Z".to_string()),
            },
        ];

        let formatted = format_transcript(&prompt);
        // Timestamps should not appear in formatted output
        assert!(!formatted.contains("2024-01-01"));
        assert!(formatted.contains("User: Question\n"));
        assert!(formatted.contains("Assistant: Answer\n"));
    }

    #[test]
    fn test_format_transcript_special_characters() {
        let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
        prompt.messages = vec![Message::User {
            text: "Text with \"quotes\" and 'apostrophes' and\ttabs\nand newlines".to_string(),
            timestamp: None,
        }];

        let formatted = format_transcript(&prompt);
        assert!(formatted.contains("\"quotes\""));
        assert!(formatted.contains("'apostrophes'"));
        assert!(formatted.contains("\t"));
    }

    #[test]
    fn test_format_transcript_unicode() {
        let mut prompt = create_test_prompt_record("test", "123", "gpt-4");
        prompt.messages = vec![Message::User {
            text: "Hello 世界 🌍 Здравствуй مرحبا".to_string(),
            timestamp: None,
        }];

        let formatted = format_transcript(&prompt);
        assert!(formatted.contains("世界"));
        assert!(formatted.contains("🌍"));
        assert!(formatted.contains("Здравствуй"));
        assert!(formatted.contains("مرحبا"));
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
    fn test_find_prompt_in_history_empty_repo() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        let result = find_prompt_in_history(tmp_repo.gitai_repo(), "any-prompt", 0);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Prompt not found in history")
        );
    }

    #[test]
    fn test_find_prompt_prompt_not_in_commit() {
        let tmp_repo = TmpRepo::new().expect("Failed to create test repo");

        // Create commit with AI checkpoint
        tmp_repo
            .write_file("test.txt", "content\n", true)
            .expect("Failed to write file");
        tmp_repo
            .trigger_checkpoint_with_ai("ai_agent", Some("gpt-4"), Some("test_tool"))
            .expect("Failed to trigger checkpoint");
        tmp_repo
            .commit_with_message("Test commit")
            .expect("Failed to commit");

        // Try to find a different prompt ID
        let result = find_prompt_in_commit(tmp_repo.gitai_repo(), "wrong-prompt-id", "HEAD");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Prompt 'wrong-prompt-id' not found in commit")
        );
    }
}
