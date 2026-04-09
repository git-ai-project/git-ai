use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::{
        agent_presets::{
            AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult, BashPreHookStrategy,
            prepare_agent_bash_pre_hook,
        },
        bash_tool::{self, BashCheckpointAction, HookEvent},
    },
    error::GitAiError,
    observability::log_error,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub struct PiPreset;

#[derive(Debug, Deserialize)]
struct PiHookInput {
    hook_event_name: String,
    session_id: String,
    session_path: String,
    cwd: String,
    model: String,
    tool_name: String,
    #[serde(default)]
    tool_name_raw: Option<String>,
    #[serde(default)]
    will_edit_filepaths: Vec<String>,
    #[serde(default)]
    edited_filepaths: Vec<String>,
    #[serde(default)]
    dirty_files: Option<HashMap<String, String>>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    tool_input: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    tool_result: Option<Value>,
}

#[derive(Debug)]
enum PiHookEvent {
    BeforeEdit,
    AfterEdit,
    BeforeCommand,
    AfterCommand,
}

impl PiHookEvent {
    fn parse(value: &str) -> Result<Self, GitAiError> {
        match value {
            "before_edit" => Ok(Self::BeforeEdit),
            "after_edit" => Ok(Self::AfterEdit),
            "before_command" => Ok(Self::BeforeCommand),
            "after_command" => Ok(Self::AfterCommand),
            other => Err(GitAiError::PresetError(format!(
                "Unsupported Pi hook_event_name: {other}"
            ))),
        }
    }
}

impl AgentCheckpointPreset for PiPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Pi preset".to_string())
        })?;

        let hook_input: PiHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {e}")))?;

        let PiHookInput {
            hook_event_name,
            session_id,
            session_path,
            cwd,
            model,
            tool_name,
            tool_name_raw,
            will_edit_filepaths,
            edited_filepaths,
            dirty_files,
            tool_use_id,
            tool_input: _,
            tool_result: _,
        } = hook_input;

        let hook_event = PiHookEvent::parse(&hook_event_name)?;
        Self::validate_tool_name(&tool_name)?;
        let is_bash = Self::is_bash_tool(&tool_name);

        // Validate event/tool consistency
        match (&hook_event, is_bash) {
            (PiHookEvent::BeforeEdit | PiHookEvent::AfterEdit, true) => {
                return Err(GitAiError::PresetError(
                    "Pi before_edit/after_edit events cannot be used with bash tools".to_string(),
                ));
            }
            (PiHookEvent::BeforeCommand | PiHookEvent::AfterCommand, false) => {
                return Err(GitAiError::PresetError(
                    "Pi before_command/after_command events require a bash tool".to_string(),
                ));
            }
            _ => {}
        }

        let model_from_hook = Self::strip_provider_prefix(model.trim());

        let mut agent_metadata = HashMap::new();
        agent_metadata.insert("session_path".to_string(), session_path.clone());
        agent_metadata.insert("tool_name".to_string(), tool_name.clone());
        if let Some(tool_name_raw) = tool_name_raw
            && !tool_name_raw.trim().is_empty()
        {
            agent_metadata.insert("tool_name_raw".to_string(), tool_name_raw);
        }

        // Build agent_id for bash events (file-edit events resolve model from transcript below)
        let bash_model = if model_from_hook.is_empty() {
            "unknown".to_string()
        } else {
            model_from_hook.clone()
        };

        match hook_event {
            PiHookEvent::BeforeCommand => {
                let agent_id = AgentId {
                    tool: "pi".to_string(),
                    id: session_id.clone(),
                    model: bash_model,
                };
                let tool_use_id_str = tool_use_id.as_deref().unwrap_or("bash");
                let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                    true,
                    Some(cwd.as_str()),
                    &session_id,
                    tool_use_id_str,
                    &agent_id,
                    Some(&agent_metadata),
                    BashPreHookStrategy::EmitHumanCheckpoint,
                )?
                .captured_checkpoint_id();
                return Ok(AgentRunResult {
                    agent_id,
                    agent_metadata: Some(agent_metadata),
                    checkpoint_kind: CheckpointKind::Human,
                    transcript: None,
                    repo_working_dir: Some(cwd),
                    edited_filepaths: None,
                    will_edit_filepaths: None,
                    dirty_files: None,
                    captured_checkpoint_id: pre_hook_captured_id,
                });
            }
            PiHookEvent::AfterCommand => {
                let agent_id = AgentId {
                    tool: "pi".to_string(),
                    id: session_id.clone(),
                    model: bash_model,
                };
                let tool_use_id_str = tool_use_id.as_deref().unwrap_or("bash");
                let bash_result = bash_tool::handle_bash_tool(
                    HookEvent::PostToolUse,
                    Path::new(&cwd),
                    &session_id,
                    tool_use_id_str,
                );
                let edited_filepaths = match &bash_result {
                    Ok(r) => match &r.action {
                        BashCheckpointAction::Checkpoint(paths) => Some(paths.clone()),
                        BashCheckpointAction::NoChanges
                        | BashCheckpointAction::Fallback
                        | BashCheckpointAction::TakePreSnapshot => None,
                    },
                    Err(e) => {
                        crate::utils::debug_log(&format!("Pi bash post-hook error: {}", e));
                        None
                    }
                };
                let bash_captured_checkpoint_id = bash_result
                    .as_ref()
                    .ok()
                    .and_then(|r| r.captured_checkpoint.as_ref())
                    .map(|info| info.capture_id.clone());
                return Ok(AgentRunResult {
                    agent_id,
                    agent_metadata: Some(agent_metadata),
                    checkpoint_kind: CheckpointKind::AiAgent,
                    transcript: None,
                    repo_working_dir: Some(cwd),
                    edited_filepaths,
                    will_edit_filepaths: None,
                    dirty_files: None,
                    captured_checkpoint_id: bash_captured_checkpoint_id,
                });
            }
            _ => {}
        }

        // File-edit path (BeforeEdit / AfterEdit)
        let (checkpoint_kind, edited_filepaths, will_edit_filepaths, transcript, parsed_model) =
            match hook_event {
                PiHookEvent::BeforeEdit => {
                    if will_edit_filepaths.is_empty() {
                        return Err(GitAiError::PresetError(
                            "Pi before_edit payload requires non-empty will_edit_filepaths"
                                .to_string(),
                        ));
                    }

                    (
                        CheckpointKind::Human,
                        None,
                        Some(will_edit_filepaths),
                        None,
                        None,
                    )
                }
                PiHookEvent::AfterEdit => {
                    if edited_filepaths.is_empty() {
                        return Err(GitAiError::PresetError(
                            "Pi after_edit payload requires non-empty edited_filepaths".to_string(),
                        ));
                    }

                    let (transcript, parsed_model) =
                        match Self::transcript_and_model_from_pi_session(&session_path) {
                            Ok(result) => result,
                            Err(e) => {
                                eprintln!("[Warning] Failed to parse Pi session JSONL: {e}");
                                log_error(
                                    &e,
                                    Some(serde_json::json!({
                                        "agent_tool": "pi",
                                        "operation": "transcript_and_model_from_pi_session"
                                    })),
                                );
                                (AiTranscript::new(), None)
                            }
                        };

                    (
                        CheckpointKind::AiAgent,
                        Some(edited_filepaths),
                        None,
                        Some(transcript),
                        parsed_model,
                    )
                }
                // Already handled above and returned early
                PiHookEvent::BeforeCommand | PiHookEvent::AfterCommand => unreachable!(),
            };

        let final_model = parsed_model
            .filter(|model| !model.trim().is_empty())
            .or_else(|| {
                if model_from_hook.is_empty() {
                    None
                } else {
                    Some(model_from_hook.clone())
                }
            })
            .unwrap_or_else(|| "unknown".to_string());

        Ok(AgentRunResult {
            agent_id: AgentId {
                tool: "pi".to_string(),
                id: session_id,
                model: final_model,
            },
            agent_metadata: Some(agent_metadata),
            checkpoint_kind,
            transcript,
            repo_working_dir: Some(cwd),
            edited_filepaths,
            will_edit_filepaths,
            dirty_files,
            captured_checkpoint_id: None,
        })
    }
}

impl PiPreset {
    /// Strip provider prefix from model strings (e.g. "anthropic/claude-opus-4-5" → "claude-opus-4-5").
    fn strip_provider_prefix(model: &str) -> String {
        match model.rsplit_once('/') {
            Some((_, name)) if !name.is_empty() => name.to_string(),
            _ => model.to_string(),
        }
    }

    fn is_bash_tool(tool_name: &str) -> bool {
        bash_tool::classify_tool(bash_tool::Agent::Pi, tool_name)
            == bash_tool::ToolClass::Bash
    }

    fn validate_tool_name(tool_name: &str) -> Result<(), GitAiError> {
        match bash_tool::classify_tool(bash_tool::Agent::Pi, tool_name) {
            bash_tool::ToolClass::FileEdit | bash_tool::ToolClass::Bash => Ok(()),
            bash_tool::ToolClass::Skip => Err(GitAiError::PresetError(format!(
                "Unsupported Pi tool_name: {tool_name}"
            ))),
        }
    }

    pub fn transcript_and_model_from_pi_session(
        session_path: impl AsRef<Path>,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let content = std::fs::read_to_string(session_path.as_ref()).map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to read Pi session file {}: {e}",
                session_path.as_ref().display()
            ))
        })?;

        let mut transcript = AiTranscript::new();
        let mut latest_model: Option<String> = None;
        let mut saw_session_header = false;

        for (index, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            let entry: Value = serde_json::from_str(line).map_err(|e| {
                GitAiError::Generic(format!(
                    "Failed to parse Pi session JSONL line {} in {}: {e}",
                    index + 1,
                    session_path.as_ref().display()
                ))
            })?;

            match entry.get("type").and_then(Value::as_str) {
                Some("session") => {
                    saw_session_header = true;
                }
                Some("message") => {
                    Self::push_message_entry(&mut transcript, &mut latest_model, &entry)?;
                }
                Some("custom")
                | Some("branch_summary")
                | Some("branchSummary")
                | Some("compaction")
                | Some("compactionSummary")
                | Some("model_change")
                | Some("thinking_level_change")
                | Some("label")
                | Some("session_info")
                | Some("custom_message") => {}
                Some(_) | None => {}
            }
        }

        if !saw_session_header {
            return Err(GitAiError::Generic(format!(
                "Pi session file {} is missing a session header",
                session_path.as_ref().display()
            )));
        }

        Ok((transcript, latest_model))
    }

    fn push_message_entry(
        transcript: &mut AiTranscript,
        latest_model: &mut Option<String>,
        entry: &Value,
    ) -> Result<(), GitAiError> {
        let message = entry
            .get("message")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                GitAiError::Generic("Pi message entry missing message object".to_string())
            })?;
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| GitAiError::Generic("Pi message entry missing role".to_string()))?;
        let timestamp = Self::message_timestamp(entry, message.get("timestamp"));

        match role {
            "user" => Self::push_user_message(transcript, message.get("content"), timestamp),
            "assistant" => {
                if let Some(model) = Self::assistant_model(message)
                    && !model.trim().is_empty()
                {
                    *latest_model = Some(model);
                }
                Self::push_assistant_message(transcript, message.get("content"), timestamp);
            }
            "toolResult" => {
                Self::push_tool_result_message(transcript, message.get("content"), timestamp);
            }
            "custom" | "branchSummary" | "compactionSummary" => {}
            _ => {}
        }

        Ok(())
    }

    fn push_user_message(
        transcript: &mut AiTranscript,
        content: Option<&Value>,
        timestamp: Option<String>,
    ) {
        match content {
            Some(Value::String(text)) => {
                if !text.trim().is_empty() {
                    transcript.add_message(Message::User {
                        text: text.to_string(),
                        timestamp,
                    });
                }
            }
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("text")
                        && let Some(text) = block.get("text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        transcript.add_message(Message::User {
                            text: text.to_string(),
                            timestamp: timestamp.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn push_assistant_message(
        transcript: &mut AiTranscript,
        content: Option<&Value>,
        timestamp: Option<String>,
    ) {
        let Some(Value::Array(blocks)) = content else {
            return;
        };

        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        transcript.add_message(Message::Assistant {
                            text: text.to_string(),
                            timestamp: timestamp.clone(),
                        });
                    }
                }
                Some("thinking") => {
                    if let Some(text) = block.get("thinking").and_then(Value::as_str)
                        && !text.trim().is_empty()
                    {
                        transcript.add_message(Message::Thinking {
                            text: text.to_string(),
                            timestamp: timestamp.clone(),
                        });
                    }
                }
                Some("toolCall") => {
                    if let Some(name) = block.get("name").and_then(Value::as_str) {
                        let input = block
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                        transcript.add_message(Message::ToolUse {
                            name: name.to_string(),
                            input,
                            timestamp: timestamp.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn push_tool_result_message(
        transcript: &mut AiTranscript,
        content: Option<&Value>,
        timestamp: Option<String>,
    ) {
        let Some(Value::Array(blocks)) = content else {
            return;
        };

        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = block.get("text").and_then(Value::as_str)
                && !text.trim().is_empty()
            {
                transcript.add_message(Message::Assistant {
                    text: text.to_string(),
                    timestamp: timestamp.clone(),
                });
            }
        }
    }

    fn assistant_model(message: &serde_json::Map<String, Value>) -> Option<String> {
        let model = message.get("model").and_then(Value::as_str)?;
        if model.trim().is_empty() {
            return None;
        }
        Some(model.to_string())
    }

    fn message_timestamp(entry: &Value, message_timestamp: Option<&Value>) -> Option<String> {
        if let Some(timestamp_ms) = message_timestamp.and_then(Value::as_i64)
            && let Some(timestamp) = DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        {
            return Some(timestamp.to_rfc3339());
        }

        entry
            .get("timestamp")
            .and_then(Value::as_str)
            .map(|value| value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::transcript::Message;
    use serde_json::json;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/")).join(name)
    }

    #[test]
    fn test_pi_payload_parsing_before_edit() {
        let session_path = fixture_path("pi-session-simple.jsonl");
        let hook_input = json!({
            "hook_event_name": "before_edit",
            "session_id": "pi-session-123",
            "session_path": session_path,
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "edit",
            "tool_name_raw": "edit",
            "will_edit_filepaths": ["/tmp/project/src/main.rs"],
            "dirty_files": {
                "/tmp/project/src/main.rs": "fn main() {}\n"
            }
        })
        .to_string();

        let result = PiPreset
            .run(AgentCheckpointFlags {
                hook_input: Some(hook_input),
            })
            .expect("Pi preset should parse before_edit payload");

        assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
        assert_eq!(result.agent_id.tool, "pi");
        assert_eq!(result.agent_id.id, "pi-session-123");
        assert_eq!(result.agent_id.model, "claude-sonnet-4-5");
        assert_eq!(result.repo_working_dir.as_deref(), Some("/tmp/project"));
        assert_eq!(
            result.will_edit_filepaths.as_deref(),
            Some(&["/tmp/project/src/main.rs".to_string()][..])
        );
        assert!(result.transcript.is_none());
        let metadata = result.agent_metadata.expect("metadata should be present");
        assert_eq!(
            metadata.get("session_path").map(String::as_str),
            Some(session_path.to_string_lossy().as_ref())
        );
        assert_eq!(metadata.get("tool_name").map(String::as_str), Some("edit"));
        assert_eq!(
            metadata.get("tool_name_raw").map(String::as_str),
            Some("edit")
        );
    }

    #[test]
    fn test_pi_rejects_unknown_tool_name() {
        let hook_input = json!({
            "hook_event_name": "after_edit",
            "session_id": "pi-session-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "unknown_tool",
            "edited_filepaths": ["/tmp/project/src/main.rs"]
        })
        .to_string();

        let error = PiPreset
            .run(AgentCheckpointFlags {
                hook_input: Some(hook_input),
            })
            .expect_err("unknown tool_name should fail");

        assert!(error
            .to_string()
            .contains("Unsupported Pi tool_name: unknown_tool"));
    }

    #[test]
    fn test_pi_rejects_bash_tool_with_edit_event() {
        let hook_input = json!({
            "hook_event_name": "after_edit",
            "session_id": "pi-session-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "bash",
            "edited_filepaths": ["/tmp/project/src/main.rs"]
        })
        .to_string();

        let error = PiPreset
            .run(AgentCheckpointFlags {
                hook_input: Some(hook_input),
            })
            .expect_err("bash tool with edit event should fail");

        assert!(error
            .to_string()
            .contains("before_edit/after_edit events cannot be used with bash"));
    }

    #[test]
    fn test_pi_rejects_edit_tool_with_command_event() {
        let hook_input = json!({
            "hook_event_name": "before_command",
            "session_id": "pi-session-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "edit",
        })
        .to_string();

        let error = PiPreset
            .run(AgentCheckpointFlags {
                hook_input: Some(hook_input),
            })
            .expect_err("edit tool with command event should fail");

        assert!(error
            .to_string()
            .contains("before_command/after_command events require a bash tool"));
    }

    #[test]
    fn test_pi_before_command_produces_human_checkpoint() {
        let hook_input = json!({
            "hook_event_name": "before_command",
            "session_id": "pi-session-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "bash",
            "tool_use_id": "tu-abc123",
        })
        .to_string();

        let result = PiPreset
            .run(AgentCheckpointFlags {
                hook_input: Some(hook_input),
            })
            .expect("before_command with bash should succeed");

        assert_eq!(result.checkpoint_kind, CheckpointKind::Human);
        assert_eq!(result.agent_id.tool, "pi");
        assert_eq!(result.agent_id.id, "pi-session-123");
        assert_eq!(result.agent_id.model, "claude-sonnet-4-5");
        assert!(result.transcript.is_none());
        assert!(result.edited_filepaths.is_none());
        assert!(result.will_edit_filepaths.is_none());
        let metadata = result.agent_metadata.expect("metadata should be present");
        assert_eq!(metadata.get("tool_name").map(String::as_str), Some("bash"));
    }

    #[test]
    fn test_pi_after_command_produces_ai_checkpoint() {
        let hook_input = json!({
            "hook_event_name": "after_command",
            "session_id": "pi-session-456",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "bash",
            "tool_use_id": "tu-abc123",
        })
        .to_string();

        let result = PiPreset
            .run(AgentCheckpointFlags {
                hook_input: Some(hook_input),
            })
            .expect("after_command with bash should succeed");

        assert_eq!(result.checkpoint_kind, CheckpointKind::AiAgent);
        assert_eq!(result.agent_id.tool, "pi");
        assert_eq!(result.agent_id.id, "pi-session-456");
        assert!(result.transcript.is_none());
    }

    #[test]
    fn test_parse_pi_simple_transcript_fixture() {
        let (transcript, model) =
            PiPreset::transcript_and_model_from_pi_session(fixture_path("pi-session-simple.jsonl"))
                .expect("simple Pi session should parse");

        assert_eq!(model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(transcript.messages().len(), 2);
        assert!(matches!(transcript.messages()[0], Message::User { .. }));
        assert!(matches!(
            transcript.messages()[1],
            Message::Assistant { .. }
        ));
    }

    #[test]
    fn test_parse_pi_tool_call_and_tool_result_fixture() {
        let (transcript, model) =
            PiPreset::transcript_and_model_from_pi_session(fixture_path("pi-session-tool.jsonl"))
                .expect("Pi tool session should parse");

        assert_eq!(model.as_deref(), Some("claude-sonnet-4-5"));
        assert!(
            transcript
                .messages()
                .iter()
                .any(|message| matches!(message, Message::ToolUse { name, .. } if name == "edit"))
        );
        assert!(transcript
            .messages()
            .iter()
            .any(|message| matches!(message, Message::Assistant { text, .. } if text.contains("Applied edit"))));
    }

    #[test]
    fn test_parse_pi_ignores_non_message_entry_kinds() {
        let (transcript, model) = PiPreset::transcript_and_model_from_pi_session(fixture_path(
            "pi-session-ignorable.jsonl",
        ))
        .expect("Pi session with ignorable entries should parse");

        assert_eq!(model.as_deref(), Some("gpt-5"));
        assert_eq!(transcript.messages().len(), 2);
        assert!(matches!(transcript.messages()[0], Message::User { .. }));
        assert!(matches!(
            transcript.messages()[1],
            Message::Assistant { .. }
        ));
    }
}
