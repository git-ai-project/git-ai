use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    error::GitAiError,
};
use chrono::{TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

pub struct AgentCheckpointFlags {
    pub hook_input: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AgentRunResult {
    pub agent_id: AgentId,
    pub agent_metadata: Option<HashMap<String, String>>,
    pub checkpoint_kind: CheckpointKind,
    pub transcript: Option<AiTranscript>,
    pub repo_working_dir: Option<String>,
    pub edited_filepaths: Option<Vec<String>>,
    pub will_edit_filepaths: Option<Vec<String>>,
    pub dirty_files: Option<HashMap<String, String>>,
}

pub trait AgentCheckpointPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError>;
}

// Claude Code to checkpoint preset
pub struct ClaudePreset;

impl AgentCheckpointPreset for ClaudePreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse claude_hook_stdin as JSON
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Claude preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        // Extract transcript_path and cwd from the JSON
        let transcript_path = hook_data
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("transcript_path not found in hook_input".to_string())
            })?;

        let _cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

        // Extract the ID from the filename
        // Example: /Users/aidancunniffe/.claude/projects/-Users-aidancunniffe-Desktop-ghq/cb947e5b-246e-4253-a953-631f7e464c6b.jsonl
        let path = Path::new(transcript_path);
        let filename = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "Could not extract filename from transcript_path".to_string(),
                )
            })?;

        // Read the file content
        let jsonl_content =
            std::fs::read_to_string(transcript_path).map_err(|e| GitAiError::IoError(e))?;

        // Parse into transcript and extract model
        let (transcript, model) = AiTranscript::from_claude_code_jsonl_with_model(&jsonl_content)
            .map_err(|e| GitAiError::JsonError(e))?;

        // The filename should be a UUID
        let agent_id = AgentId {
            tool: "claude".to_string(),
            id: filename.to_string(),
            model: model.unwrap_or_else(|| "unknown".to_string()),
        };

        // Extract file_path from tool_input if present
        let file_path_as_vec = hook_data
            .get("tool_input")
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|path| vec![path.to_string()]);

        // Check if this is a PreToolUse event (human checkpoint)
        let hook_event_name = hook_data.get("hook_event_name").and_then(|v| v.as_str());

        if hook_event_name == Some("PreToolUse") {
            // Early return for human checkpoint
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: None,
                edited_filepaths: None,
                will_edit_filepaths: file_path_as_vec,
                dirty_files: None,
            });
        }

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            // use default.
            repo_working_dir: None,
            edited_filepaths: file_path_as_vec,
            will_edit_filepaths: None,
            dirty_files: None,
        })
    }
}

// Cursor to checkpoint preset
pub struct CursorPreset;

impl AgentCheckpointPreset for CursorPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse hook_input JSON to extract workspace_roots and conversation_id
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Cursor preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        // Extract conversation_id and workspace_roots from the JSON
        let conversation_id = hook_data
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("conversation_id not found in hook_input".to_string())
            })?
            .to_string();

        let workspace_roots = hook_data
            .get("workspace_roots")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError("workspace_roots not found in hook_input".to_string())
            })?
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<String>>();

        let hook_event_name = hook_data
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("hook_event_name not found in hook_input".to_string())
            })?
            .to_string();

        // Validate hook_event_name
        if hook_event_name != "beforeSubmitPrompt" && hook_event_name != "afterFileEdit" {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected 'beforeSubmitPrompt' or 'afterFileEdit'",
                hook_event_name
            )));
        }

        let repo_working_dir = workspace_roots.first().cloned().ok_or_else(|| {
            GitAiError::PresetError("No workspace root found in hook_input".to_string())
        })?;

        if hook_event_name == "beforeSubmitPrompt" {
            // early return, we're just adding a human checkpoint.
            return Ok(AgentRunResult {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: conversation_id.clone(),
                    model: "unknown".to_string(),
                },
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(repo_working_dir),
                edited_filepaths: None,
                will_edit_filepaths: None,
                dirty_files: None,
            });
        }

        // Locate Cursor storage
        let global_db = Self::cursor_global_database_path()?;
        if !global_db.exists() {
            return Err(GitAiError::PresetError(format!(
                "Cursor global state database not found at {:?}. \
                Make sure Cursor is installed and has been used at least once. \
                Expected location: {:?}",
                global_db,
                global_db,
            )));
        }

        // Fetch the composer data and extract transcript + model + edited filepaths
        let (transcript, model) = match Self::fetch_composer_payload(&global_db, &conversation_id)
        {
            Ok(payload) => Self::transcript_data_from_composer_payload(
                &payload,
                &global_db,
                &conversation_id,
            )?
            .unwrap_or_else(|| {
                // Return empty transcript as default
                // There's a race condition causing new threads to sometimes not show up.
                // We refresh and grab all the messages in post-commit so we're ok with returning an empty (placeholder) transcript here and not throwing
                eprintln!(
                    "[Warning] Could not extract transcript from Cursor composer. Retrying at commit."
                );
                (AiTranscript::new(), "unknown".to_string())
            }),
            Err(GitAiError::PresetError(msg))
                if msg == "No conversation data found in database" =>
            {
                // Gracefully continue when the conversation hasn't been written yet due to Cursor race conditions
                eprintln!(
                    "[Warning] No conversation data found in Cursor DB for this thread. Proceeding and will re-sync at commit."
                );
                (AiTranscript::new(), "unknown".to_string())
            }
            Err(e) => return Err(e),
        };

        // Extract edited filepaths
        let mut edited_filepaths: Option<Vec<String>> = None;
        let file_path = hook_data
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !file_path.is_empty() {
            edited_filepaths = Some(vec![file_path.to_string()]);
        }

        let agent_id = AgentId {
            tool: "cursor".to_string(),
            id: conversation_id,
            model,
        };

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: Some(repo_working_dir),
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files: None,
        })
    }
}

impl CursorPreset {
    /// Fetch the latest version of a Cursor conversation from the database
    pub fn fetch_latest_cursor_conversation(
        conversation_id: &str,
    ) -> Result<Option<(AiTranscript, String)>, GitAiError> {
        let global_db = Self::cursor_global_database_path()?;
        if !global_db.exists() {
            return Ok(None);
        }

        // Fetch composer payload
        let composer_payload = Self::fetch_composer_payload(&global_db, conversation_id)?;

        // Extract transcript and model
        let transcript_data = Self::transcript_data_from_composer_payload(
            &composer_payload,
            &global_db,
            conversation_id,
        )?;

        Ok(transcript_data)
    }

    // Get the Cursor database path
    fn cursor_global_database_path() -> Result<PathBuf, GitAiError> {
        if let Ok(global_db_path) = std::env::var("GIT_AI_CURSOR_GLOBAL_DB_PATH") {
            return Ok(PathBuf::from(global_db_path));
        }
        let user_dir = Self::cursor_user_dir()?;
        let global_db = user_dir.join("globalStorage").join("state.vscdb");
        Ok(global_db)
    }

    fn cursor_user_dir() -> Result<PathBuf, GitAiError> {
        #[cfg(target_os = "windows")]
        {
            // Windows: %APPDATA%\Cursor\User
            let appdata = env::var("APPDATA")
                .map_err(|e| GitAiError::Generic(format!("APPDATA not set: {}", e)))?;
            Ok(Path::new(&appdata).join("Cursor").join("User"))
        }

        #[cfg(target_os = "macos")]
        {
            // macOS: ~/Library/Application Support/Cursor/User
            let home = env::var("HOME")
                .map_err(|e| GitAiError::Generic(format!("HOME not set: {}", e)))?;
            Ok(Path::new(&home)
                .join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User"))
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            Err(GitAiError::PresetError(
                "Cursor is only supported on Windows and macOS platforms".to_string(),
            ))
        }
    }

    fn open_sqlite_readonly(path: &Path) -> Result<Connection, GitAiError> {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| GitAiError::Generic(format!("Failed to open {:?}: {}", path, e)))
    }

    pub fn fetch_composer_payload(
        global_db_path: &Path,
        composer_id: &str,
    ) -> Result<serde_json::Value, GitAiError> {
        let conn = Self::open_sqlite_readonly(global_db_path)?;

        // Look for the composer data in cursorDiskKV
        let key_pattern = format!("composerData:{}", composer_id);
        let mut stmt = conn
            .prepare("SELECT value FROM cursorDiskKV WHERE key = ?")
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;

        let mut rows = stmt
            .query([&key_pattern])
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;

        if let Ok(Some(row)) = rows.next() {
            let value_text: String = row
                .get(0)
                .map_err(|e| GitAiError::Generic(format!("Failed to read value: {}", e)))?;

            let data = serde_json::from_str::<serde_json::Value>(&value_text)
                .map_err(|e| GitAiError::Generic(format!("Failed to parse JSON: {}", e)))?;

            return Ok(data);
        }

        Err(GitAiError::PresetError(
            "No conversation data found in database".to_string(),
        ))
    }

    pub fn transcript_data_from_composer_payload(
        data: &serde_json::Value,
        global_db_path: &Path,
        composer_id: &str,
    ) -> Result<Option<(AiTranscript, String)>, GitAiError> {
        // Only support fullConversationHeadersOnly (bubbles format) - the current Cursor format
        // All conversations since April 2025 use this format exclusively
        let conv = data
            .get("fullConversationHeadersOnly")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "Conversation uses unsupported legacy format. Only conversations created after April 2025 are supported.".to_string()
                )
            })?;

        let mut transcript = AiTranscript::new();
        let mut model = None;

        for header in conv.iter() {
            if let Some(bubble_id) = header.get("bubbleId").and_then(|v| v.as_str()) {
                if let Ok(Some(bubble_content)) =
                    Self::fetch_bubble_content_from_db(global_db_path, composer_id, bubble_id)
                {
                    // Get bubble created at (ISO 8601 UTC string)
                    let bubble_created_at = bubble_content
                        .get("createdAt")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // Extract model from bubble (first value wins)
                    if model.is_none() {
                        if let Some(model_info) = bubble_content.get("modelInfo") {
                            if let Some(model_name) =
                                model_info.get("modelName").and_then(|v| v.as_str())
                            {
                                model = Some(model_name.to_string());
                            }
                        }
                    }

                    // Extract text from bubble
                    if let Some(text) = bubble_content.get("text").and_then(|v| v.as_str()) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            let role = header.get("type").and_then(|v| v.as_i64()).unwrap_or(0);
                            if role == 1 {
                                transcript.add_message(Message::user(
                                    trimmed.to_string(),
                                    bubble_created_at.clone(),
                                ));
                            } else {
                                transcript.add_message(Message::assistant(
                                    trimmed.to_string(),
                                    bubble_created_at.clone(),
                                ));
                            }
                        }
                    }

                    // Handle tool calls and edits
                    if let Some(tool_former_data) = bubble_content.get("toolFormerData") {
                        let tool_name = tool_former_data
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let raw_args_str = tool_former_data
                            .get("rawArgs")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        let raw_args_json = serde_json::from_str::<serde_json::Value>(raw_args_str)
                            .unwrap_or(serde_json::Value::Null);
                        match tool_name {
                            "edit_file" => {
                                let target_file =
                                    raw_args_json.get("target_file").and_then(|v| v.as_str());
                                transcript.add_message(Message::tool_use(
                                    tool_name.to_string(),
                                    // Explicitly clear out everything other than target_file (renamed to file_path for consistency in git-ai) (too much data in rawArgs)
                                    serde_json::json!({ "file_path": target_file.unwrap_or("") }),
                                ));
                            }
                            "apply_patch"
                            | "edit_file_v2_apply_patch"
                            | "search_replace"
                            | "edit_file_v2_search_replace"
                            | "write"
                            | "MultiEdit" => {
                                let file_path =
                                    raw_args_json.get("file_path").and_then(|v| v.as_str());
                                transcript.add_message(Message::tool_use(
                                    tool_name.to_string(),
                                    // Explicitly clear out everything other than file_path (too much data in rawArgs)
                                    serde_json::json!({ "file_path": file_path.unwrap_or("") }),
                                ));
                            }
                            "codebase_search" | "grep" | "read_file" | "web_search"
                            | "run_terminal_cmd" | "glob_file_search" | "todo_write"
                            | "file_search" | "grep_search" | "list_dir" | "ripgrep" => {
                                transcript.add_message(Message::tool_use(
                                    tool_name.to_string(),
                                    raw_args_json,
                                ));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        if !transcript.messages.is_empty() {
            Ok(Some((transcript, model.unwrap_or("unknown".to_string()))))
        } else {
            Ok(None)
        }
    }

    pub fn fetch_bubble_content_from_db(
        global_db_path: &Path,
        composer_id: &str,
        bubble_id: &str,
    ) -> Result<Option<serde_json::Value>, GitAiError> {
        let conn = Self::open_sqlite_readonly(global_db_path)?;

        // Look for bubble data in cursorDiskKV with pattern bubbleId:composerId:bubbleId
        let bubble_pattern = format!("bubbleId:{}:{}", composer_id, bubble_id);
        let mut stmt = conn
            .prepare("SELECT value FROM cursorDiskKV WHERE key = ?")
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;

        let mut rows = stmt
            .query([&bubble_pattern])
            .map_err(|e| GitAiError::Generic(format!("Query failed: {}", e)))?;

        if let Ok(Some(row)) = rows.next() {
            let value_text: String = row
                .get(0)
                .map_err(|e| GitAiError::Generic(format!("Failed to read value: {}", e)))?;

            let data = serde_json::from_str::<serde_json::Value>(&value_text)
                .map_err(|e| GitAiError::Generic(format!("Failed to parse JSON: {}", e)))?;

            return Ok(Some(data));
        }

        Ok(None)
    }
}

pub struct GithubCopilotPreset;

impl AgentCheckpointPreset for GithubCopilotPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse hook_input JSON to extract chat session information
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for GitHub Copilot preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let chat_session_path = hook_data
            .get("chatSessionPath")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("chatSessionPath not found in hook_input".to_string())
            })?;
        
        let agent_metadata = HashMap::from([
            ("chat_session_path".to_string(), chat_session_path.to_string()),
        ]);

        // Accept either chatSessionId (old) or sessionId (from VS Code extension)
        let chat_session_id = hook_data
            .get("chatSessionId")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("sessionId").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();

        // Extract dirtyFiles if available
        let dirty_files = hook_data
            .get("dirtyFiles")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(key, value)| {
                        value
                            .as_str()
                            .map(|content| (key.clone(), content.to_string()))
                    })
                    .collect::<HashMap<String, String>>()
            });

        // Required working directory provided by the extension
        let repo_working_dir: String = hook_data
            .get("workspaceFolder")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "workspaceFolder not found in hook_input for GitHub Copilot preset".to_string(),
                )
            })?
            .to_string();

        // Read the Copilot chat session JSON (ignore errors)
        let (transcript, detected_model, edited_filepaths) =
            GithubCopilotPreset::transcript_and_model_from_copilot_session_json(chat_session_path)
                .map(|(t, m, f)| (Some(t), m, f))
                .unwrap_or_else(|e| {
                    // TODO Log error to sentry (JSON exists but invalid)
                    eprintln!(
                        "[Warning] Failed to parse GitHub Copilot chat session JSON from {} (will update transcript at commit): {}",
                        chat_session_path,
                        e
                    );
                    (None, None, None)
                });

        let agent_id = AgentId {
            tool: "github-copilot".to_string(),
            id: chat_session_id,
            model: detected_model.unwrap_or_else(|| "unknown".to_string()),
        };

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: Some(agent_metadata),
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript,
            repo_working_dir: Some(repo_working_dir),
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files,
        })
    }
}

impl GithubCopilotPreset {
    /// Translate a GitHub Copilot chat session JSON file into an AiTranscript, optional model, and edited filepaths.
    /// Returns an empty transcript if running in Codespaces or Remote Containers.
    pub fn transcript_and_model_from_copilot_session_json(
        session_json_path: &str,
    ) -> Result<(AiTranscript, Option<String>, Option<Vec<String>>), GitAiError> {
        // Check if running in Codespaces or Remote Containers - if so, return empty transcript
        let is_codespaces = env::var("CODESPACES").ok().as_deref() == Some("true");
        let is_remote_containers = env::var("REMOTE_CONTAINERS").ok().as_deref() == Some("true");

        if is_codespaces || is_remote_containers {
            return Ok((AiTranscript::new(), None, Some(Vec::new())));
        }

        // Read the session JSON file
        let session_json_str = std::fs::read_to_string(session_json_path)
            .map_err(|e| GitAiError::IoError(e))?;

        let session_json: serde_json::Value =
            serde_json::from_str(&session_json_str).map_err(|e| GitAiError::JsonError(e))?;

        // Extract the requests array which represents the conversation from start to finish
        let requests = session_json
            .get("requests")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "requests array not found in Copilot chat session".to_string(),
                )
            })?;

        let mut transcript = AiTranscript::new();
        let mut detected_model: Option<String> = None;
        let mut edited_filepaths: Vec<String> = Vec::new();

        for request in requests {
            // Parse the human timestamp once per request (unix ms and RFC3339)
            let user_ts_ms = request.get("timestamp").and_then(|v| v.as_i64());
            let user_ts_rfc3339 = user_ts_ms.and_then(|ms| {
                Utc.timestamp_millis_opt(ms)
                    .single()
                    .map(|dt| dt.to_rfc3339())
            });

            // Add the human's message
            if let Some(user_text) = request
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(|v| v.as_str())
            {
                let trimmed = user_text.trim();
                if !trimmed.is_empty() {
                    transcript.add_message(Message::User {
                        text: trimmed.to_string(),
                        timestamp: user_ts_rfc3339.clone(),
                    });
                }
            }

            // Process the agent's response items: tool invocations, edits, and text
            if let Some(response_items) = request.get("response").and_then(|v| v.as_array()) {
                let mut assistant_text_accumulator = String::new();

                for item in response_items {
                    // Capture tool invocations and other structured actions as tool_use
                    if let Some(kind) = item.get("kind").and_then(|v| v.as_str()) {
                        match kind {
                            // Primary tool invocation entries
                            "toolInvocationSerialized" => {
                                let tool_name = item
                                    .get("toolId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool");

                                // Normalize invocationMessage to a string
                                let inv_msg = item.get("invocationMessage").and_then(|im| {
                                    if let Some(s) = im.as_str() {
                                        Some(s.to_string())
                                    } else if im.is_object() {
                                        im.get("value")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    } else {
                                        None
                                    }
                                });

                                if let Some(msg) = inv_msg {
                                    transcript.add_message(Message::tool_use(
                                        tool_name.to_string(),
                                        serde_json::Value::String(msg),
                                    ));
                                }
                            }
                            // Other structured response elements worth capturing
                            "textEditGroup" => {
                                // Extract file path from textEditGroup
                                if let Some(uri_obj) = item.get("uri") {
                                    let path_opt = uri_obj
                                        .get("fsPath")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                        .or_else(|| {
                                            uri_obj
                                                .get("path")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        });
                                    if let Some(p) = path_opt {
                                        if !edited_filepaths.contains(&p) {
                                            edited_filepaths.push(p);
                                        }
                                    }
                                }
                                transcript
                                    .add_message(Message::tool_use(kind.to_string(), item.clone()));
                            }
                            "prepareToolInvocation" => {
                                transcript
                                    .add_message(Message::tool_use(kind.to_string(), item.clone()));
                            }
                            // codeblockUri should contribute a visible mention like @path, not a tool_use
                            "codeblockUri" => {
                                let path_opt = item
                                    .get("uri")
                                    .and_then(|u| {
                                        u.get("fsPath")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                            .or_else(|| {
                                                u.get("path")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string())
                                            })
                                    })
                                    .or_else(|| {
                                        item.get("fsPath")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .or_else(|| {
                                        item.get("path")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    });
                                if let Some(p) = path_opt {
                                    let mention = format!("@{}", p);
                                    if !assistant_text_accumulator.is_empty() {
                                        assistant_text_accumulator.push(' ');
                                    }
                                    assistant_text_accumulator.push_str(&mention);
                                }
                            }
                            // inlineReference should contribute a visible mention like @path, not a tool_use
                            "inlineReference" => {
                                let path_opt = item.get("inlineReference").and_then(|ir| {
                                    // Try nested uri.fsPath or uri.path
                                    ir.get("uri")
                                        .and_then(|u| u.get("fsPath"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                        .or_else(|| {
                                            ir.get("uri")
                                                .and_then(|u| u.get("path"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        // Or top-level fsPath / path on inlineReference
                                        .or_else(|| {
                                            ir.get("fsPath")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .or_else(|| {
                                            ir.get("path")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                });
                                if let Some(p) = path_opt {
                                    let mention = format!("@{}", p);
                                    if !assistant_text_accumulator.is_empty() {
                                        assistant_text_accumulator.push(' ');
                                    }
                                    assistant_text_accumulator.push_str(&mention);
                                }
                            }
                            _ => {}
                        }
                    }

                    // Accumulate visible assistant text snippets
                    if let Some(val) = item.get("value").and_then(|v| v.as_str()) {
                        let t = val.trim();
                        if !t.is_empty() {
                            if !assistant_text_accumulator.is_empty() {
                                assistant_text_accumulator.push(' ');
                            }
                            assistant_text_accumulator.push_str(t);
                        }
                    }
                }

                if !assistant_text_accumulator.trim().is_empty() {
                    // Set assistant timestamp to user_ts + totalElapsed if available
                    let assistant_ts = request
                        .get("result")
                        .and_then(|r| r.get("timings"))
                        .and_then(|t| t.get("totalElapsed"))
                        .and_then(|v| v.as_i64())
                        .and_then(|elapsed| user_ts_ms.map(|ums| ums + elapsed))
                        .and_then(|ms| {
                            Utc.timestamp_millis_opt(ms)
                                .single()
                                .map(|dt| dt.to_rfc3339())
                        });

                    transcript.add_message(Message::Assistant {
                        text: assistant_text_accumulator.trim().to_string(),
                        timestamp: assistant_ts,
                    });
                }
            }

            // Detect model from request metadata if not yet set (uses first modelId seen)
            if detected_model.is_none() {
                if let Some(model_id) = request.get("modelId").and_then(|v| v.as_str()) {
                    detected_model = Some(model_id.to_string());
                }
            }
        }

        Ok((transcript, detected_model, Some(edited_filepaths)))
    }
}

pub struct AiTabPreset;

#[derive(Debug, Deserialize)]
struct AiTabHookInput {
    hook_event_name: String,
    tool: String,
    model: String,
    repo_working_dir: Option<String>,
    will_edit_filepaths: Option<Vec<String>>,
    edited_filepaths: Option<Vec<String>>,
    completion_id: Option<String>,
    dirty_files: Option<HashMap<String, String>>,
}

impl AgentCheckpointPreset for AiTabPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for ai_tab preset".to_string())
        })?;

        let hook_input: AiTabHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let AiTabHookInput {
            hook_event_name,
            tool,
            model,
            repo_working_dir,
            will_edit_filepaths,
            edited_filepaths,
            completion_id,
            dirty_files,
        } = hook_input;

        if hook_event_name != "before_edit" && hook_event_name != "after_edit" {
            return Err(GitAiError::PresetError(format!(
                "Unsupported hook_event_name '{}' for ai_tab preset (expected 'before_edit' or 'after_edit')",
                hook_event_name
            )));
        }

        let tool = tool.trim().to_string();
        if tool.is_empty() {
            return Err(GitAiError::PresetError(
                "tool must be a non-empty string for ai_tab preset".to_string(),
            ));
        }

        let model = model.trim().to_string();
        if model.is_empty() {
            return Err(GitAiError::PresetError(
                "model must be a non-empty string for ai_tab preset".to_string(),
            ));
        }

        let repo_working_dir = repo_working_dir
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let agent_id = AgentId {
            tool,
            id: format!("ai_tab-{}", completion_id.unwrap_or_else(|| Utc::now().timestamp_millis().to_string())),
            model,
        };

        if hook_event_name == "before_edit" {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir,
                edited_filepaths: None,
                will_edit_filepaths,
                dirty_files,
            });
        }

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: None,
            checkpoint_kind: CheckpointKind::AiTab,
            transcript: None,
            repo_working_dir,
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files,
        })
    }
}
