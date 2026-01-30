use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::agent_presets::{AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult},
    error::GitAiError,
    observability::log_error,
};
use chrono::DateTime;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct OpenCodePreset;

/// Hook input from OpenCode plugin
#[derive(Debug, Deserialize)]
struct OpenCodeHookInput {
    hook_event_name: String,
    session_id: String,
    cwd: String,
    tool_input: Option<ToolInput>,
}

#[derive(Debug, Deserialize)]
struct ToolInput {
    #[serde(rename = "filePath")]
    file_path: Option<String>,
}

/// Message metadata from message/{session_id}/{msg_id}.json
#[derive(Debug, Deserialize)]
struct OpenCodeMessage {
    id: String,
    #[serde(rename = "sessionID")]
    #[allow(dead_code)]
    session_id: String,
    role: String, // "user" | "assistant"
    time: OpenCodeTime,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenCodeTime {
    created: i64,
    #[allow(dead_code)]
    completed: Option<i64>,
}

/// Tool state object containing status and nested data
#[derive(Debug, Deserialize)]
struct ToolState {
    #[allow(dead_code)]
    status: Option<String>,
    input: Option<serde_json::Value>,
    #[allow(dead_code)]
    output: Option<serde_json::Value>,
    #[allow(dead_code)]
    title: Option<String>,
    #[allow(dead_code)]
    metadata: Option<serde_json::Value>,
    time: Option<OpenCodePartTime>,
}

/// Part content from part/{msg_id}/{prt_id}.json
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum OpenCodePart {
    Text {
        #[serde(rename = "messageID")]
        #[allow(dead_code)]
        message_id: String,
        text: String,
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        synthetic: Option<bool>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    Tool {
        #[serde(rename = "messageID")]
        #[allow(dead_code)]
        message_id: String,
        tool: String,
        #[serde(rename = "callID")]
        #[allow(dead_code)]
        call_id: String,
        state: Option<ToolState>,
        input: Option<serde_json::Value>,
        #[allow(dead_code)]
        output: Option<serde_json::Value>,
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepStart {
        #[serde(rename = "messageID")]
        #[allow(dead_code)]
        message_id: String,
        #[allow(dead_code)]
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepFinish {
        #[serde(rename = "messageID")]
        #[allow(dead_code)]
        message_id: String,
        #[allow(dead_code)]
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct OpenCodePartTime {
    start: i64,
    #[allow(dead_code)]
    end: Option<i64>,
}

impl AgentCheckpointPreset for OpenCodePreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for OpenCode preset".to_string())
        })?;

        let hook_input: OpenCodeHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let OpenCodeHookInput {
            hook_event_name,
            session_id,
            cwd,
            tool_input,
        } = hook_input;

        // Extract file_path from tool_input if present
        let file_path_as_vec = tool_input
            .and_then(|ti| ti.file_path)
            .map(|path| vec![path]);

        // Determine storage path - check for test override first
        let storage_path = if let Ok(test_path) = std::env::var("GIT_AI_OPENCODE_STORAGE_PATH") {
            PathBuf::from(test_path)
        } else {
            Self::opencode_storage_path()?
        };

        // Fetch transcript and model from storage
        let (transcript, model) =
            match Self::transcript_and_model_from_storage(&storage_path, &session_id) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse OpenCode storage: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "opencode",
                            "operation": "transcript_and_model_from_storage"
                        })),
                    );
                    (AiTranscript::new(), None)
                }
            };

        let agent_id = AgentId {
            tool: "opencode".to_string(),
            id: session_id.clone(),
            model: model.unwrap_or_else(|| "unknown".to_string()),
        };

        // Store session_id in metadata for post-commit refetch
        let mut agent_metadata = HashMap::new();
        agent_metadata.insert("session_id".to_string(), session_id);
        // Store test storage path if set, for subprocess access
        if let Ok(test_path) = std::env::var("GIT_AI_OPENCODE_STORAGE_PATH") {
            agent_metadata.insert("__test_storage_path".to_string(), test_path);
        }

        // Check if this is a PreToolUse event (human checkpoint)
        if hook_event_name == "PreToolUse" {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(cwd),
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
            repo_working_dir: Some(cwd),
            edited_filepaths: file_path_as_vec,
            will_edit_filepaths: None,
            dirty_files: None,
        })
    }
}

impl OpenCodePreset {
    /// Get the OpenCode storage path based on platform
    pub fn opencode_storage_path() -> Result<PathBuf, GitAiError> {
        #[cfg(target_os = "macos")]
        {
            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            Ok(home.join(".local").join("share").join("opencode").join("storage"))
        }

        #[cfg(target_os = "linux")]
        {
            // Try XDG_DATA_HOME first, then fall back to ~/.local/share
            if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
                Ok(PathBuf::from(xdg_data).join("opencode").join("storage"))
            } else {
                let home = dirs::home_dir().ok_or_else(|| {
                    GitAiError::Generic("Could not determine home directory".to_string())
                })?;
                Ok(home.join(".local").join("share").join("opencode").join("storage"))
            }
        }

        #[cfg(target_os = "windows")]
        {
            let local_app_data = std::env::var("LOCALAPPDATA").map_err(|e| {
                GitAiError::Generic(format!("LOCALAPPDATA not set: {}", e))
            })?;
            Ok(PathBuf::from(local_app_data).join("opencode").join("storage"))
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(GitAiError::PresetError(
                "OpenCode storage path not supported on this platform".to_string(),
            ))
        }
    }

    /// Public API for fetching transcript from session_id (uses default storage path)
    pub fn transcript_and_model_from_session(
        session_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let storage_path = Self::opencode_storage_path()?;
        Self::transcript_and_model_from_storage(&storage_path, session_id)
    }

    /// Fetch transcript and model from OpenCode storage for a given session
    pub fn transcript_and_model_from_storage(
        storage_path: &Path,
        session_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        if !storage_path.exists() {
            return Err(GitAiError::PresetError(format!(
                "OpenCode storage path does not exist: {:?}",
                storage_path
            )));
        }

        // Read all messages for this session
        let messages = Self::read_session_messages(storage_path, session_id)?;
        if messages.is_empty() {
            return Ok((AiTranscript::new(), None));
        }

        // Sort messages by creation time
        let mut sorted_messages = messages;
        sorted_messages.sort_by_key(|m| m.time.created);

        let mut transcript = AiTranscript::new();
        let mut model: Option<String> = None;

        for message in &sorted_messages {
            // Extract model from first assistant message
            if model.is_none() && message.role == "assistant" {
                if let (Some(provider_id), Some(model_id)) =
                    (&message.provider_id, &message.model_id)
                {
                    model = Some(format!("{}/{}", provider_id, model_id));
                } else if let Some(model_id) = &message.model_id {
                    model = Some(model_id.clone());
                }
            }

            // Read parts for this message
            let parts = Self::read_message_parts(storage_path, &message.id)?;

            // Convert Unix ms to RFC3339 timestamp
            let timestamp = DateTime::from_timestamp_millis(message.time.created)
                .map(|dt| dt.to_rfc3339());

            for part in parts {
                match part {
                    OpenCodePart::Text { text, .. } => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            if message.role == "user" {
                                transcript.add_message(Message::User {
                                    text: trimmed.to_string(),
                                    timestamp: timestamp.clone(),
                                });
                            } else if message.role == "assistant" {
                                transcript.add_message(Message::Assistant {
                                    text: trimmed.to_string(),
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                    }
                    OpenCodePart::Tool {
                        tool,
                        input,
                        state,
                        ..
                    } => {
                        // Only include tool calls from assistant messages
                        if message.role == "assistant" {
                            // Try part input first, then state.input as fallback
                            let tool_input = input
                                .or_else(|| state.and_then(|s| s.input))
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            transcript.add_message(Message::ToolUse {
                                name: tool,
                                input: tool_input,
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                    OpenCodePart::StepStart { .. } | OpenCodePart::StepFinish { .. } => {
                        // Skip step markers - they don't contribute to the transcript
                    }
                    OpenCodePart::Unknown => {
                        // Skip unknown part types
                    }
                }
            }
        }

        Ok((transcript, model))
    }

    /// Read all message files for a session
    fn read_session_messages(
        storage_path: &Path,
        session_id: &str,
    ) -> Result<Vec<OpenCodeMessage>, GitAiError> {
        let message_dir = storage_path.join("message").join(session_id);
        if !message_dir.exists() {
            return Ok(Vec::new());
        }

        let mut messages = Vec::new();

        let entries = std::fs::read_dir(&message_dir).map_err(|e| {
            GitAiError::IoError(e)
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| GitAiError::IoError(e))?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "json") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        match serde_json::from_str::<OpenCodeMessage>(&content) {
                            Ok(message) => messages.push(message),
                            Err(e) => {
                                eprintln!(
                                    "[Warning] Failed to parse OpenCode message file {:?}: {}",
                                    path, e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[Warning] Failed to read OpenCode message file {:?}: {}",
                            path, e
                        );
                    }
                }
            }
        }

        Ok(messages)
    }

    /// Read all part files for a message
    fn read_message_parts(
        storage_path: &Path,
        message_id: &str,
    ) -> Result<Vec<OpenCodePart>, GitAiError> {
        let part_dir = storage_path.join("part").join(message_id);
        if !part_dir.exists() {
            return Ok(Vec::new());
        }

        let mut parts: Vec<(i64, OpenCodePart)> = Vec::new();

        let entries = std::fs::read_dir(&part_dir).map_err(|e| {
            GitAiError::IoError(e)
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| GitAiError::IoError(e))?;
            let path = entry.path();

            if path.extension().map_or(false, |ext| ext == "json") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        match serde_json::from_str::<OpenCodePart>(&content) {
                            Ok(part) => {
                                // Extract creation time for sorting (handles optional time fields)
                                let created = match &part {
                                    OpenCodePart::Text { time, .. } => {
                                        time.as_ref().map(|t| t.start).unwrap_or(0)
                                    }
                                    OpenCodePart::Tool { time, state, .. } => {
                                        // Try part time first, then state time as fallback
                                        time.as_ref()
                                            .map(|t| t.start)
                                            .or_else(|| {
                                                state
                                                    .as_ref()
                                                    .and_then(|s| s.time.as_ref())
                                                    .map(|t| t.start)
                                            })
                                            .unwrap_or(0)
                                    }
                                    OpenCodePart::StepStart { time, .. } => {
                                        time.as_ref().map(|t| t.start).unwrap_or(0)
                                    }
                                    OpenCodePart::StepFinish { time, .. } => {
                                        time.as_ref().map(|t| t.start).unwrap_or(0)
                                    }
                                    OpenCodePart::Unknown => 0,
                                };
                                parts.push((created, part));
                            }
                            Err(e) => {
                                eprintln!(
                                    "[Warning] Failed to parse OpenCode part file {:?}: {}",
                                    path, e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[Warning] Failed to read OpenCode part file {:?}: {}",
                            path, e
                        );
                    }
                }
            }
        }

        // Sort parts by creation time
        parts.sort_by_key(|(created, _)| *created);

        Ok(parts.into_iter().map(|(_, part)| part).collect())
    }
}
