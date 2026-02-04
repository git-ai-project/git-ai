use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    error::GitAiError,
    observability::log_error,
};
use chrono::{TimeZone, Utc};
use dirs;
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

        // Parse into transcript and extract model
        let (transcript, model) =
            match ClaudePreset::transcript_and_model_from_claude_code_jsonl(transcript_path) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Claude JSONL: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "claude",
                            "operation": "transcript_and_model_from_claude_code_jsonl"
                        })),
                    );
                    (
                        crate::authorship::transcript::AiTranscript::new(),
                        Some("unknown".to_string()),
                    )
                }
            };

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

        // Store transcript_path in metadata
        let agent_metadata =
            HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]);

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
            agent_metadata: Some(agent_metadata),
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

impl ClaudePreset {
    /// Parse a Claude Code JSONL file into a transcript and extract model info
    pub fn transcript_and_model_from_claude_code_jsonl(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let jsonl_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let mut transcript = AiTranscript::new();
        let mut model = None;

        for line in jsonl_content.lines() {
            if !line.trim().is_empty() {
                // Parse the raw JSONL entry
                let raw_entry: serde_json::Value = serde_json::from_str(line)?;
                let timestamp = raw_entry["timestamp"].as_str().map(|s| s.to_string());

                // Extract model from assistant messages if we haven't found it yet
                if model.is_none() && raw_entry["type"].as_str() == Some("assistant")
                    && let Some(model_str) = raw_entry["message"]["model"].as_str() {
                        model = Some(model_str.to_string());
                    }

                // Extract messages based on the type
                match raw_entry["type"].as_str() {
                    Some("user") => {
                        // Handle user messages
                        if let Some(content) = raw_entry["message"]["content"].as_str() {
                            if !content.trim().is_empty() {
                                transcript.add_message(Message::User {
                                    text: content.to_string(),
                                    timestamp: timestamp.clone(),
                                });
                            }
                        } else if let Some(content_array) =
                            raw_entry["message"]["content"].as_array()
                        {
                            // Handle user messages with content array
                            for item in content_array {
                                // Skip tool_result items - those are system-generated responses, not human input
                                if item["type"].as_str() == Some("tool_result") {
                                    continue;
                                }
                                // Handle text content blocks from actual user input
                                if item["type"].as_str() == Some("text")
                                    && let Some(text) = item["text"].as_str()
                                        && !text.trim().is_empty() {
                                            transcript.add_message(Message::User {
                                                text: text.to_string(),
                                                timestamp: timestamp.clone(),
                                            });
                                        }
                            }
                        }
                    }
                    Some("assistant") => {
                        // Handle assistant messages
                        if let Some(content_array) = raw_entry["message"]["content"].as_array() {
                            for item in content_array {
                                match item["type"].as_str() {
                                    Some("text") => {
                                        if let Some(text) = item["text"].as_str()
                                            && !text.trim().is_empty() {
                                                transcript.add_message(Message::Assistant {
                                                    text: text.to_string(),
                                                    timestamp: timestamp.clone(),
                                                });
                                            }
                                    }
                                    Some("thinking") => {
                                        if let Some(thinking) = item["thinking"].as_str()
                                            && !thinking.trim().is_empty() {
                                                transcript.add_message(Message::Assistant {
                                                    text: thinking.to_string(),
                                                    timestamp: timestamp.clone(),
                                                });
                                            }
                                    }
                                    Some("tool_use") => {
                                        if let (Some(name), Some(_input)) =
                                            (item["name"].as_str(), item["input"].as_object())
                                        {
                                            transcript.add_message(Message::ToolUse {
                                                name: name.to_string(),
                                                input: item["input"].clone(),
                                                timestamp: timestamp.clone(),
                                            });
                                        }
                                    }
                                    _ => continue, // Skip unknown content types
                                }
                            }
                        }
                    }
                    _ => continue, // Skip unknown message types
                }
            }
        }

        Ok((transcript, model))
    }
}

pub struct GeminiPreset;

impl AgentCheckpointPreset for GeminiPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse claude_hook_stdin as JSON
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Gemini preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("session_id not found in hook_input".to_string())
            })?;

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

        // Parse into transcript and extract model
        let (transcript, model) =
            match GeminiPreset::transcript_and_model_from_gemini_json(transcript_path) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Gemini JSON: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "gemini",
                            "operation": "transcript_and_model_from_gemini_json"
                        })),
                    );
                    (
                        crate::authorship::transcript::AiTranscript::new(),
                        Some("unknown".to_string()),
                    )
                }
            };

        // The filename should be a UUID
        let agent_id = AgentId {
            tool: "gemini".to_string(),
            id: session_id.to_string(),
            model: model.unwrap_or_else(|| "unknown".to_string()),
        };

        // Extract file_path from tool_input if present
        let file_path_as_vec = hook_data
            .get("tool_input")
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|path| vec![path.to_string()]);

        // Store transcript_path in metadata
        let agent_metadata =
            HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]);

        // Check if this is a PreToolUse event (human checkpoint)
        let hook_event_name = hook_data.get("hook_event_name").and_then(|v| v.as_str());

        if hook_event_name == Some("BeforeTool") {
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
            agent_metadata: Some(agent_metadata),
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

impl GeminiPreset {
    /// Parse a Gemini JSON file into a transcript and extract model info
    pub fn transcript_and_model_from_gemini_json(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let json_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let conversation: serde_json::Value =
            serde_json::from_str(&json_content).map_err(GitAiError::JsonError)?;

        let messages = conversation
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError("messages array not found in Gemini JSON".to_string())
            })?;

        let mut transcript = AiTranscript::new();
        let mut model = None;

        for message in messages {
            let message_type = match message.get("type").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => {
                    // Skip messages without a type field
                    continue;
                }
            };

            let timestamp = message
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            match message_type {
                "user" => {
                    // Handle user messages - content can be a string
                    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                        let trimmed = content.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::User {
                                text: trimmed.to_string(),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                }
                "gemini" => {
                    // Extract model from gemini messages if we haven't found it yet
                    if model.is_none()
                        && let Some(model_str) = message.get("model").and_then(|v| v.as_str()) {
                            model = Some(model_str.to_string());
                        }

                    // Handle assistant text content - content can be a string
                    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                        let trimmed = content.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::Assistant {
                                text: trimmed.to_string(),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }

                    // Handle tool calls
                    if let Some(tool_calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                        for tool_call in tool_calls {
                            if let Some(name) = tool_call.get("name").and_then(|v| v.as_str()) {
                                // Extract args, defaulting to empty object if not present
                                let args = tool_call.get("args").cloned().unwrap_or_else(|| {
                                    serde_json::Value::Object(serde_json::Map::new())
                                });

                                let tool_timestamp = tool_call
                                    .get("timestamp")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());

                                transcript.add_message(Message::ToolUse {
                                    name: name.to_string(),
                                    input: args,
                                    timestamp: tool_timestamp,
                                });
                            }
                        }
                    }
                }
                _ => {
                    // Skip unknown message types (info, error, warning, etc.)
                    continue;
                }
            }
        }

        Ok((transcript, model))
    }
}

pub struct ContinueCliPreset;

impl AgentCheckpointPreset for ContinueCliPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse hook_input as JSON
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Continue CLI preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("session_id not found in hook_input".to_string())
            })?;

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

        // Extract model from hook_input (required)
        let model = hook_data
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                eprintln!("[Warning] Continue CLI: 'model' field not found in hook_input, defaulting to 'unknown'");
                eprintln!("[Debug] hook_data keys: {:?}", hook_data.as_object().map(|obj| obj.keys().collect::<Vec<_>>()));
                "unknown".to_string()
            });

        eprintln!("[Debug] Continue CLI using model: {}", model);

        // Parse transcript from JSON file
        let transcript = match ContinueCliPreset::transcript_from_continue_json(transcript_path) {
            Ok(transcript) => transcript,
            Err(e) => {
                eprintln!("[Warning] Failed to parse Continue CLI JSON: {e}");
                log_error(
                    &e,
                    Some(serde_json::json!({
                        "agent_tool": "continue-cli",
                        "operation": "transcript_from_continue_json"
                    })),
                );
                crate::authorship::transcript::AiTranscript::new()
            }
        };

        // The session_id is the unique identifier for this conversation
        let agent_id = AgentId {
            tool: "continue-cli".to_string(),
            id: session_id.to_string(),
            model,
        };

        // Extract file_path from tool_input if present
        let file_path_as_vec = hook_data
            .get("tool_input")
            .and_then(|ti| ti.get("file_path"))
            .and_then(|v| v.as_str())
            .map(|path| vec![path.to_string()]);

        // Store transcript_path in metadata
        let agent_metadata =
            HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]);

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
            agent_metadata: Some(agent_metadata),
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

impl ContinueCliPreset {
    /// Parse a Continue CLI JSON file into a transcript
    pub fn transcript_from_continue_json(
        transcript_path: &str,
    ) -> Result<AiTranscript, GitAiError> {
        let json_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let conversation: serde_json::Value =
            serde_json::from_str(&json_content).map_err(GitAiError::JsonError)?;

        let history = conversation
            .get("history")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError("history array not found in Continue CLI JSON".to_string())
            })?;

        let mut transcript = AiTranscript::new();

        for history_item in history {
            // Extract the message from the history item
            let message = match history_item.get("message") {
                Some(m) => m,
                None => continue, // Skip items without a message
            };

            let role = match message.get("role").and_then(|v| v.as_str()) {
                Some(r) => r,
                None => continue, // Skip messages without a role
            };

            // Extract timestamp from message if available
            let timestamp = message
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            match role {
                "user" => {
                    // Handle user messages - content is a string
                    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                        let trimmed = content.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::User {
                                text: trimmed.to_string(),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                }
                "assistant" => {
                    // Handle assistant text content
                    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                        let trimmed = content.trim();
                        if !trimmed.is_empty() {
                            transcript.add_message(Message::Assistant {
                                text: trimmed.to_string(),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }

                    // Handle tool calls from the message
                    if let Some(tool_calls) = message.get("toolCalls").and_then(|v| v.as_array()) {
                        for tool_call in tool_calls {
                            if let Some(function) = tool_call.get("function") {
                                let tool_name = function
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");

                                // Parse the arguments JSON string
                                let args = if let Some(args_str) =
                                    function.get("arguments").and_then(|v| v.as_str())
                                {
                                    serde_json::from_str::<serde_json::Value>(args_str)
                                        .unwrap_or_else(|_| {
                                            serde_json::Value::Object(serde_json::Map::new())
                                        })
                                } else {
                                    serde_json::Value::Object(serde_json::Map::new())
                                };

                                let tool_timestamp = tool_call
                                    .get("timestamp")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());

                                transcript.add_message(Message::ToolUse {
                                    name: tool_name.to_string(),
                                    input: args,
                                    timestamp: tool_timestamp,
                                });
                            }
                        }
                    }
                }
                _ => {
                    // Skip unknown roles
                    continue;
                }
            }
        }

        Ok(transcript)
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
            .filter_map(|v| v.as_str().map(Self::normalize_cursor_path))
            .collect::<Vec<String>>();

        let hook_event_name = hook_data
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("hook_event_name not found in hook_input".to_string())
            })?
            .to_string();

        // Extract model from hook input (Cursor provides this directly)
        let model = hook_data
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Validate hook_event_name
        if hook_event_name != "beforeSubmitPrompt" && hook_event_name != "afterFileEdit" {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected 'beforeSubmitPrompt' or 'afterFileEdit'",
                hook_event_name
            )));
        }

        let file_path = hook_data
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(Self::normalize_cursor_path)
            .unwrap_or_default();

        let repo_working_dir = if !file_path.is_empty() {
            workspace_roots
                .iter()
                .find(|root| {
                    let root_str = root.as_str();
                    file_path.starts_with(root_str)
                        && (file_path.len() == root_str.len()
                            || file_path[root_str.len()..].starts_with('/')
                            || file_path[root_str.len()..].starts_with('\\')
                            || root_str.ends_with('/')
                            || root_str.ends_with('\\'))
                })
                .cloned()
                .or_else(|| workspace_roots.first().cloned())
                .ok_or_else(|| {
                    GitAiError::PresetError("No workspace root found in hook_input".to_string())
                })?
        } else {
            workspace_roots.first().cloned().ok_or_else(|| {
                GitAiError::PresetError("No workspace root found in hook_input".to_string())
            })?
        };

        if hook_event_name == "beforeSubmitPrompt" {
            // early return, we're just adding a human checkpoint.
            return Ok(AgentRunResult {
                agent_id: AgentId {
                    tool: "cursor".to_string(),
                    id: conversation_id.clone(),
                    model: model.clone(),
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
                global_db, global_db,
            )));
        }

        // Fetch the composer data and extract transcript (model is now from hook input, not DB)
        let transcript = match Self::fetch_composer_payload(&global_db, &conversation_id) {
            Ok(payload) => Self::transcript_data_from_composer_payload(
                &payload,
                &global_db,
                &conversation_id,
            )?
            .map(|(transcript, _db_model)| transcript)
            .unwrap_or_else(|| {
                // Return empty transcript as default
                // There's a race condition causing new threads to sometimes not show up.
                // We refresh and grab all the messages in post-commit so we're ok with returning an empty (placeholder) transcript here and not throwing
                eprintln!(
                    "[Warning] Could not extract transcript from Cursor composer. Retrying at commit."
                );
                AiTranscript::new()
            }),
            Err(GitAiError::PresetError(msg))
                if msg == "No conversation data found in database" =>
            {
                // Gracefully continue when the conversation hasn't been written yet due to Cursor race conditions
                eprintln!(
                    "[Warning] No conversation data found in Cursor DB for this thread. Proceeding and will re-sync at commit."
                );
                AiTranscript::new()
            }
            Err(e) => return Err(e),
        };

        let edited_filepaths = if !file_path.is_empty() {
            Some(vec![file_path.to_string()])
        } else {
            None
        };

        let agent_id = AgentId {
            tool: "cursor".to_string(),
            id: conversation_id,
            model,
        };

        // Store cursor database path in metadata for refetching during post-commit.
        // This is only needed when GIT_AI_CURSOR_GLOBAL_DB_PATH env var is set (i.e., in tests),
        // because the env var isn't passed to git hook subprocesses.
        let agent_metadata = if std::env::var("GIT_AI_CURSOR_GLOBAL_DB_PATH").is_ok() {
            Some(HashMap::from([(
                "__test_cursor_db_path".to_string(),
                global_db.to_string_lossy().to_string(),
            )]))
        } else {
            None
        };

        Ok(AgentRunResult {
            agent_id,
            agent_metadata,
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
    /// Normalize Windows paths that Cursor sends in Unix-style format.
    ///
    /// On Windows, Cursor sometimes sends paths like `/c:/Users/...` instead of `C:\Users\...`.
    /// This function converts those paths to proper Windows format.
    #[cfg(windows)]
    fn normalize_cursor_path(path: &str) -> String {
        // Check for pattern like /c:/ or /C:/ at the start
        // e.g. "/c:/Users/foo" -> "C:\Users\foo"
        let mut chars = path.chars();
        if chars.next() == Some('/') {
            if let (Some(drive), Some(':')) = (chars.next(), chars.next()) {
                if drive.is_ascii_alphabetic() {
                    let rest: String = chars.collect();
                    // Convert forward slashes to backslashes for Windows
                    let normalized_rest = rest.replace('/', "\\");
                    return format!("{}:{}", drive.to_ascii_uppercase(), normalized_rest);
                }
            }
        }
        // No conversion needed
        path.to_string()
    }

    #[cfg(not(windows))]
    fn normalize_cursor_path(path: &str) -> String {
        // On non-Windows platforms, no conversion needed
        path.to_string()
    }

    /// Fetch the latest version of a Cursor conversation from the database
    pub fn fetch_latest_cursor_conversation(
        conversation_id: &str,
    ) -> Result<Option<(AiTranscript, String)>, GitAiError> {
        let global_db = Self::cursor_global_database_path()?;
        Self::fetch_cursor_conversation_from_db(&global_db, conversation_id)
    }

    /// Fetch a Cursor conversation from a specific database path
    pub fn fetch_cursor_conversation_from_db(
        db_path: &std::path::Path,
        conversation_id: &str,
    ) -> Result<Option<(AiTranscript, String)>, GitAiError> {
        if !db_path.exists() {
            return Ok(None);
        }

        // Fetch composer payload
        let composer_payload = Self::fetch_composer_payload(db_path, conversation_id)?;

        // Extract transcript and model
        let transcript_data = Self::transcript_data_from_composer_payload(
            &composer_payload,
            db_path,
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
            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            Ok(home
                .join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User"))
        }

        #[cfg(target_os = "linux")]
        {
            // Linux: ~/.config/Cursor/User
            let config_dir = dirs::config_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine user config directory".to_string())
            })?;
            Ok(config_dir
                .join("Cursor")
                .join("User"))
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
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
            if let Some(bubble_id) = header.get("bubbleId").and_then(|v| v.as_str())
                && let Ok(Some(bubble_content)) =
                    Self::fetch_bubble_content_from_db(global_db_path, composer_id, bubble_id)
                {
                    // Get bubble created at (ISO 8601 UTC string)
                    let bubble_created_at = bubble_content
                        .get("createdAt")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // Extract model from bubble (first value wins)
                    if model.is_none()
                        && let Some(model_info) = bubble_content.get("modelInfo")
                            && let Some(model_name) =
                                model_info.get("modelName").and_then(|v| v.as_str())
                            {
                                model = Some(model_name.to_string());
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

        // Extract hook_event_name to determine checkpoint type
        // Fallback to "after_edit" if not set (for older versions of the VS Code extension)
        let hook_event_name = hook_data
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .unwrap_or("after_edit");

        // Validate hook_event_name
        if hook_event_name != "before_edit" && hook_event_name != "after_edit" {
            return Err(GitAiError::PresetError(format!(
                "Invalid hook_event_name: {}. Expected 'before_edit' or 'after_edit'",
                hook_event_name
            )));
        }

        // Required working directory provided by the extension
        // Accept snake_case (new) with fallback to camelCase (old) for backward compatibility
        let repo_working_dir: String = hook_data
            .get("workspace_folder")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("workspaceFolder").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "workspace_folder or workspaceFolder not found in hook_input for GitHub Copilot preset".to_string(),
                )
            })?
            .to_string();

        // Extract dirty_files if available (snake_case with fallback to camelCase)
        let dirty_files = hook_data
            .get("dirty_files")
            .and_then(|v| v.as_object())
            .or_else(|| hook_data.get("dirtyFiles").and_then(|v| v.as_object()))
            .map(|obj| {
                obj.iter()
                    .filter_map(|(key, value)| {
                        value
                            .as_str()
                            .map(|content| (key.clone(), content.to_string()))
                    })
                    .collect::<HashMap<String, String>>()
            });

        // Handle before_edit (human checkpoint)
        if hook_event_name == "before_edit" {
            // Extract will_edit_filepaths (required for human checkpoints)
            let will_edit_filepaths = hook_data
                .get("will_edit_filepaths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<String>>()
                })
                .ok_or_else(|| {
                    GitAiError::PresetError(
                        "will_edit_filepaths is required for before_edit hook_event_name"
                            .to_string(),
                    )
                })?;

            if will_edit_filepaths.is_empty() {
                return Err(GitAiError::PresetError(
                    "will_edit_filepaths cannot be empty for before_edit hook_event_name"
                        .to_string(),
                ));
            }

            return Ok(AgentRunResult {
                agent_id: AgentId {
                    tool: "human".to_string(),
                    id: "human".to_string(),
                    model: "human".to_string(),
                },
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(repo_working_dir),
                edited_filepaths: None,
                will_edit_filepaths: Some(will_edit_filepaths),
                dirty_files,
            });
        }

        // Handle after_edit (AI checkpoint)
        // Accept snake_case (new) with fallback to camelCase (old) for backward compatibility
        let chat_session_path = hook_data
            .get("chat_session_path")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("chatSessionPath").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                GitAiError::PresetError(
                    "chat_session_path or chatSessionPath not found in hook_input for after_edit"
                        .to_string(),
                )
            })?;

        let agent_metadata = HashMap::from([(
            "chat_session_path".to_string(),
            chat_session_path.to_string(),
        )]);

        // Accept snake_case (new) with fallback to camelCase (old) for backward compatibility
        // Accept either chat_session_id/session_id (new) or chatSessionId/sessionId (old)
        let chat_session_id = hook_data
            .get("chat_session_id")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("session_id").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("chatSessionId").and_then(|v| v.as_str()))
            .or_else(|| hook_data.get("sessionId").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();

        // TODO Make edited_filepaths required in future versions (after old extensions are updated)
        // Optionally take edited_filepaths from hook_data if present (from extension)
        let edited_filepaths = hook_data
            .get("edited_filepaths")
            .and_then(|val| val.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<String>>()
            });

        // Read the Copilot chat session JSON (ignore errors)
        let (transcript, detected_model, detected_edited_filepaths) =
            GithubCopilotPreset::transcript_and_model_from_copilot_session_json(chat_session_path)
                .map(|(t, m, f)| (Some(t), m, f))
                .unwrap_or_else(|e| {
                    eprintln!(
                        "[Warning] Failed to parse GitHub Copilot chat session JSON from {} (will update transcript at commit): {}",
                        chat_session_path,
                        e
                    );
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "github-copilot",
                            "operation": "transcript_and_model_from_copilot_session_json",
                            "note": "JSON exists but invalid"
                        })),
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
            // TODO Remove detected_edited_filepaths once edited_filepaths is required in future versions (after old extensions are updated)
            edited_filepaths: edited_filepaths.or(detected_edited_filepaths),
            will_edit_filepaths: None,
            dirty_files,
        })
    }
}

impl AgentCheckpointPreset for DroidPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        // Parse hook_input JSON from Droid
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Droid preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        // Extract common fields from Droid hook input
        // Note: Droid may use either snake_case or camelCase field names
        // session_id is optional - generate a fallback if not present
        let session_id = hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("sessionId").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                format!(
                    "droid-{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                )
            });

        // transcript_path is optional - Droid may not always provide it
        let transcript_path = hook_data
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("transcriptPath").and_then(|v| v.as_str()));

        let cwd = hook_data
            .get("cwd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GitAiError::PresetError("cwd not found in hook_input".to_string()))?;

        let hook_event_name = hook_data
            .get("hookEventName")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("hook_event_name").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                GitAiError::PresetError("hookEventName not found in hook_input".to_string())
            })?;

        // Extract tool_name and tool_input for tool-related events
        let tool_name = hook_data
            .get("tool_name")
            .and_then(|v| v.as_str())
            .or_else(|| hook_data.get("toolName").and_then(|v| v.as_str()));

        // Extract file_path from tool_input if present
        let tool_input = hook_data
            .get("tool_input")
            .or_else(|| hook_data.get("toolInput"));

        let mut file_path_as_vec = tool_input
            .and_then(|ti| {
                ti.get("file_path")
                    .or_else(|| ti.get("filePath"))
                    .and_then(|v| v.as_str())
                    .map(|path| vec![path.to_string()])
            });

        // For ApplyPatch, extract file paths from the patch text
        // Patch format contains lines like: *** Update File: <path>
        if file_path_as_vec.is_none() && tool_name == Some("ApplyPatch") {
            let mut paths = Vec::new();

            // Try extracting from tool_input patch text
            if let Some(ti) = tool_input
                && let Some(patch_text) = ti.as_str().or_else(|| ti.get("patch").and_then(|v| v.as_str())) {
                    for line in patch_text.lines() {
                        let trimmed = line.trim();
                        if let Some(path) = trimmed.strip_prefix("*** Update File: ")
                            .or_else(|| trimmed.strip_prefix("*** Add File: "))
                        {
                            paths.push(path.trim().to_string());
                        }
                    }
                }

            // For PostToolUse, also try parsing tool_response for file_path
            if paths.is_empty() && hook_event_name == "PostToolUse"
                && let Some(tool_response) = hook_data.get("tool_response").or_else(|| hook_data.get("toolResponse")) {
                    // tool_response might be a JSON string or an object
                    let response_obj = if let Some(s) = tool_response.as_str() {
                        serde_json::from_str::<serde_json::Value>(s).ok()
                    } else {
                        Some(tool_response.clone())
                    };
                    if let Some(obj) = response_obj
                        && let Some(path) = obj.get("file_path").or_else(|| obj.get("filePath")).and_then(|v| v.as_str()) {
                            paths.push(path.to_string());
                        }
                }

            if !paths.is_empty() {
                file_path_as_vec = Some(paths);
            }
        }

        // Resolve transcript and settings paths:
        // 1. Use transcript_path from hook input if provided
        // 2. Otherwise derive from session_id + cwd
        let (resolved_transcript_path, resolved_settings_path) = if let Some(tp) = transcript_path
        {
            // Derive settings path as sibling of transcript_path
            let settings = tp.replace(".jsonl", ".settings.json");
            (tp.to_string(), settings)
        } else {
            let (jsonl_p, settings_p) =
                DroidPreset::droid_session_paths(&session_id, cwd);
            (
                jsonl_p.to_string_lossy().to_string(),
                settings_p.to_string_lossy().to_string(),
            )
        };

        // Parse the Droid transcript JSONL file
        let transcript =
            match DroidPreset::transcript_and_model_from_droid_jsonl(&resolved_transcript_path) {
                Ok((transcript, _model)) => transcript,
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Droid JSONL: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "droid",
                            "operation": "transcript_and_model_from_droid_jsonl"
                        })),
                    );
                    crate::authorship::transcript::AiTranscript::new()
                }
            };

        // Extract model from settings.json
        let model = match DroidPreset::model_from_droid_settings_json(&resolved_settings_path) {
            Ok(m) => m.unwrap_or_else(|| "unknown".to_string()),
            Err(_) => "unknown".to_string(),
        };

        let agent_id = AgentId {
            tool: "droid".to_string(),
            id: session_id,
            model,
        };

        // Store both paths in metadata
        let mut agent_metadata = HashMap::new();
        agent_metadata.insert(
            "transcript_path".to_string(),
            resolved_transcript_path.clone(),
        );
        agent_metadata.insert(
            "settings_path".to_string(),
            resolved_settings_path.clone(),
        );
        if let Some(name) = tool_name {
            agent_metadata.insert("tool_name".to_string(), name.to_string());
        }

        // Check if this is a PreToolUse event (human checkpoint)
        if hook_event_name == "PreToolUse" {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(cwd.to_string()),
                edited_filepaths: None,
                will_edit_filepaths: file_path_as_vec,
                dirty_files: None,
            });
        }

        // PostToolUse event - AI checkpoint
        Ok(AgentRunResult {
            agent_id,
            agent_metadata: Some(agent_metadata),
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: Some(cwd.to_string()),
            edited_filepaths: file_path_as_vec,
            will_edit_filepaths: None,
            dirty_files: None,
        })
    }
}

impl DroidPreset {
    /// Parse a Droid JSONL transcript file into a transcript.
    /// Droid JSONL uses the same nested format as Claude Code:
    /// `{"type":"message","timestamp":"...","message":{"role":"user|assistant","content":[...]}}`
    /// Model is NOT stored in the JSONL — it comes from the companion .settings.json file.
    pub fn transcript_and_model_from_droid_jsonl(
        transcript_path: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let jsonl_content =
            std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
        let mut transcript = AiTranscript::new();

        for line in jsonl_content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let raw_entry: serde_json::Value = serde_json::from_str(line)?;

            // Only process "message" entries; skip session_start, todo_state, etc.
            if raw_entry["type"].as_str() != Some("message") {
                continue;
            }

            let timestamp = raw_entry["timestamp"].as_str().map(|s| s.to_string());

            let message = &raw_entry["message"];
            let role = match message["role"].as_str() {
                Some(r) => r,
                None => continue,
            };

            match role {
                "user" => {
                    if let Some(content_array) = message["content"].as_array() {
                        for item in content_array {
                            // Skip tool_result items — those are system-generated responses
                            if item["type"].as_str() == Some("tool_result") {
                                continue;
                            }
                            if item["type"].as_str() == Some("text")
                                && let Some(text) = item["text"].as_str()
                                    && !text.trim().is_empty() {
                                        transcript.add_message(Message::User {
                                            text: text.to_string(),
                                            timestamp: timestamp.clone(),
                                        });
                                    }
                        }
                    } else if let Some(content) = message["content"].as_str()
                        && !content.trim().is_empty() {
                            transcript.add_message(Message::User {
                                text: content.to_string(),
                                timestamp: timestamp.clone(),
                            });
                        }
                }
                "assistant" => {
                    if let Some(content_array) = message["content"].as_array() {
                        for item in content_array {
                            match item["type"].as_str() {
                                Some("text") => {
                                    if let Some(text) = item["text"].as_str()
                                        && !text.trim().is_empty() {
                                            transcript.add_message(Message::Assistant {
                                                text: text.to_string(),
                                                timestamp: timestamp.clone(),
                                            });
                                        }
                                }
                                Some("thinking") => {
                                    if let Some(thinking) = item["thinking"].as_str()
                                        && !thinking.trim().is_empty() {
                                            transcript.add_message(Message::Assistant {
                                                text: thinking.to_string(),
                                                timestamp: timestamp.clone(),
                                            });
                                        }
                                }
                                Some("tool_use") => {
                                    if let (Some(name), Some(_input)) =
                                        (item["name"].as_str(), item["input"].as_object())
                                    {
                                        transcript.add_message(Message::ToolUse {
                                            name: name.to_string(),
                                            input: item["input"].clone(),
                                            timestamp: timestamp.clone(),
                                        });
                                    }
                                }
                                _ => continue,
                            }
                        }
                    }
                }
                _ => continue,
            }
        }

        // Model is not in the JSONL — return None
        Ok((transcript, None))
    }

    /// Read the model from a Droid .settings.json file
    pub fn model_from_droid_settings_json(
        settings_path: &str,
    ) -> Result<Option<String>, GitAiError> {
        let content =
            std::fs::read_to_string(settings_path).map_err(GitAiError::IoError)?;
        let settings: serde_json::Value =
            serde_json::from_str(&content).map_err(GitAiError::JsonError)?;
        Ok(settings["model"].as_str().map(|s| s.to_string()))
    }

    /// Derive JSONL and settings.json paths from a session_id and cwd.
    /// Droid stores sessions at ~/.factory/sessions/{encoded_cwd}/{session_id}.jsonl
    /// where encoded_cwd replaces '/' with '-'.
    pub fn droid_session_paths(session_id: &str, cwd: &str) -> (PathBuf, PathBuf) {
        let encoded_cwd = cwd.replace('/', "-");
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".factory")
            .join("sessions")
            .join(&encoded_cwd);
        let jsonl_path = base.join(format!("{}.jsonl", session_id));
        let settings_path = base.join(format!("{}.settings.json", session_id));
        (jsonl_path, settings_path)
    }
}

impl GithubCopilotPreset {
    /// Translate a GitHub Copilot chat session JSON file into an AiTranscript, optional model, and edited filepaths.
    /// Returns an empty transcript if running in Codespaces or Remote Containers.
    #[allow(clippy::type_complexity)]
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
        let session_json_str =
            std::fs::read_to_string(session_json_path).map_err(GitAiError::IoError)?;

        let session_json: serde_json::Value =
            serde_json::from_str(&session_json_str).map_err(GitAiError::JsonError)?;

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
                                    if let Some(p) = path_opt
                                        && !edited_filepaths.contains(&p) {
                                            edited_filepaths.push(p);
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
            if detected_model.is_none()
                && let Some(model_id) = request.get("modelId").and_then(|v| v.as_str()) {
                    detected_model = Some(model_id.to_string());
                }
        }

        Ok((transcript, detected_model, Some(edited_filepaths)))
    }
}

pub struct AiTabPreset;

// Droid (Factory) to checkpoint preset
pub struct DroidPreset;

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
            id: format!(
                "ai_tab-{}",
                completion_id.unwrap_or_else(|| Utc::now().timestamp_millis().to_string())
            ),
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
