use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    error::GitAiError,
    observability::log_error,
};
use std::collections::HashMap;
use std::path::Path;

use super::agent_presets::{AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult};

pub struct CodeBuddyPreset;

impl AgentCheckpointPreset for CodeBuddyPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let stdin_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for CodeBuddy preset".to_string())
        })?;

        let hook_data: serde_json::Value = serde_json::from_str(&stdin_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = hook_data
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GitAiError::PresetError("session_id not found in hook_input".to_string())
            })?
            .to_string();

        let hook_event_name = hook_data
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let model = hook_data
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Extract file_path from tool_input.filePath (camelCase, CodeBuddy convention)
        let file_path_as_vec = hook_data
            .get("tool_input")
            .and_then(|ti| {
                ti.get("filePath")
                    .or_else(|| ti.get("file_path"))
                    .and_then(|v| v.as_str())
            })
            .map(|path| vec![path.to_string()]);

        let transcript_path = hook_data
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let agent_id = AgentId {
            tool: "codebuddy".to_string(),
            id: session_id,
            model,
        };

        // PreToolUse → human checkpoint (snapshot before AI edits)
        if hook_event_name == "PreToolUse" {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                // cwd is always "/" from CodeBuddy CN — don't use it.
                // File-based repo detection in git_ai_handlers.rs handles this.
                repo_working_dir: None,
                edited_filepaths: None,
                will_edit_filepaths: file_path_as_vec,
                dirty_files: None,
                captured_checkpoint_id: None,
            });
        }

        // PostToolUse → AI checkpoint
        let transcript = match &transcript_path {
            Some(tp) => match CodeBuddyPreset::transcript_from_codebuddy_session(tp) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[Warning] Failed to parse CodeBuddy transcript: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "codebuddy",
                            "operation": "transcript_from_codebuddy_session"
                        })),
                    );
                    AiTranscript::new()
                }
            },
            None => AiTranscript::new(),
        };

        let agent_metadata =
            transcript_path.map(|tp| HashMap::from([("transcript_path".to_string(), tp)]));

        Ok(AgentRunResult {
            agent_id,
            agent_metadata,
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: None,
            edited_filepaths: file_path_as_vec,
            will_edit_filepaths: None,
            dirty_files: None,
            captured_checkpoint_id: None,
        })
    }
}

impl CodeBuddyPreset {
    /// Parse a CodeBuddy CN transcript from its directory-based format.
    ///
    /// CodeBuddy CN stores transcripts as:
    /// ```text
    /// {session_id}/
    ///   index.json        — message index: {"messages": [{"id":"...","role":"user",...}, ...]}
    ///   messages/
    ///     {id}.json        — per-message content with double-encoded `message` field
    /// ```
    ///
    /// The `transcript_path` argument should point to the `index.json` file.
    pub fn transcript_from_codebuddy_session(
        transcript_path: &str,
    ) -> Result<AiTranscript, GitAiError> {
        let index_path = Path::new(transcript_path);
        let session_dir = index_path.parent().ok_or_else(|| {
            GitAiError::PresetError(format!(
                "Cannot determine session directory from transcript_path: {}",
                transcript_path
            ))
        })?;
        let messages_dir = session_dir.join("messages");

        let index_content = std::fs::read_to_string(index_path).map_err(GitAiError::IoError)?;
        let index_json: serde_json::Value = serde_json::from_str(&index_content)?;

        let message_entries = index_json
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                GitAiError::PresetError("index.json missing 'messages' array".to_string())
            })?;

        let mut transcript = AiTranscript::new();

        for entry in message_entries {
            let id = match entry.get("id").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => continue,
            };
            let role = entry.get("role").and_then(|v| v.as_str()).unwrap_or("");

            // Read the individual message file
            let msg_path = messages_dir.join(format!("{}.json", id));
            let msg_content = match std::fs::read_to_string(&msg_path) {
                Ok(c) => c,
                Err(_) => continue, // Skip missing message files
            };
            let msg_json: serde_json::Value = match serde_json::from_str(&msg_content) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // The `message` field is a JSON-encoded string that must be parsed again
            let inner = match msg_json.get("message").and_then(|v| v.as_str()) {
                Some(json_str) => {
                    match serde_json::from_str::<serde_json::Value>(json_str) {
                        Ok(v) => v,
                        Err(_) => {
                            // Fallback: treat the raw string as plain text
                            serde_json::json!({"content": [{"type": "text", "text": json_str}]})
                        }
                    }
                }
                None => {
                    // If `message` is not a string, try using it directly as an object
                    match msg_json.get("message") {
                        Some(v) => v.clone(),
                        None => continue,
                    }
                }
            };

            // Extract text from content array
            let text = Self::extract_text_from_content(&inner);
            if text.is_empty() {
                continue;
            }

            match role {
                "user" => {
                    transcript.add_message(Message::User {
                        text,
                        timestamp: None,
                    });
                }
                "assistant" => {
                    transcript.add_message(Message::Assistant {
                        text,
                        timestamp: None,
                    });
                }
                _ => continue,
            }
        }

        Ok(transcript)
    }

    /// Extract text content from a parsed message's `content` field.
    /// Handles both `content` as a string and as an array of blocks.
    fn extract_text_from_content(inner: &serde_json::Value) -> String {
        if let Some(content_str) = inner.get("content").and_then(|v| v.as_str()) {
            return content_str.to_string();
        }
        if let Some(content_array) = inner.get("content").and_then(|v| v.as_array()) {
            let texts: Vec<&str> = content_array
                .iter()
                .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect();
            return texts.join("\n");
        }
        String::new()
    }
}
