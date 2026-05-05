use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext, TranscriptFormat, TranscriptSource,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

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

impl PiPreset {
    fn strip_provider_prefix(model: &str) -> String {
        match model.rsplit_once('/') {
            Some((_, name)) if !name.is_empty() => name.to_string(),
            _ => model.to_string(),
        }
    }

    fn is_bash_tool(tool_name: &str) -> bool {
        bash_tool::classify_tool(Agent::Pi, tool_name) == ToolClass::Bash
    }

    fn validate_tool_name(tool_name: &str) -> Result<(), GitAiError> {
        match bash_tool::classify_tool(Agent::Pi, tool_name) {
            ToolClass::FileEdit | ToolClass::Bash => Ok(()),
            ToolClass::Skip => Err(GitAiError::PresetError(format!(
                "Unsupported Pi tool_name: {tool_name}"
            ))),
        }
    }
}

impl AgentPreset for PiPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let hook_input: PiHookInput = serde_json::from_str(hook_input)
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

        let model_stripped = Self::strip_provider_prefix(model.trim());
        let model_final = if model_stripped.is_empty() {
            "unknown".to_string()
        } else {
            model_stripped
        };

        // Build agent metadata
        let mut metadata = HashMap::new();
        metadata.insert("session_path".to_string(), session_path.clone());
        metadata.insert("tool_name".to_string(), tool_name.clone());
        if let Some(tool_name_raw) = tool_name_raw
            && !tool_name_raw.trim().is_empty()
        {
            metadata.insert("tool_name_raw".to_string(), tool_name_raw);
        }

        let tool_use_id_str = tool_use_id.as_deref().unwrap_or("bash").to_string();

        let context = PresetContext {
            agent_id: AgentId {
                tool: "pi".to_string(),
                id: session_id.clone(),
                model: model_final,
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&cwd),
            metadata,
        };

        let transcript_source = {
            let path = PathBuf::from(&session_path);
            Some(TranscriptSource {
                path,
                format: TranscriptFormat::PiJsonl,
                session_id: context.session_id.clone(),
                external_thread_id: None,
            })
        };

        let dirty =
            dirty_files.map(|df| df.into_iter().map(|(k, v)| (PathBuf::from(k), v)).collect());

        let event = match hook_event {
            PiHookEvent::BeforeEdit => {
                if will_edit_filepaths.is_empty() {
                    return Err(GitAiError::PresetError(
                        "Pi before_edit payload requires non-empty will_edit_filepaths".to_string(),
                    ));
                }
                ParsedHookEvent::PreFileEdit(PreFileEdit {
                    context,
                    file_paths: will_edit_filepaths.into_iter().map(PathBuf::from).collect(),
                    dirty_files: dirty,
                    tool_use_id: tool_use_id.clone(),
                })
            }
            PiHookEvent::AfterEdit => {
                if edited_filepaths.is_empty() {
                    return Err(GitAiError::PresetError(
                        "Pi after_edit payload requires non-empty edited_filepaths".to_string(),
                    ));
                }
                ParsedHookEvent::PostFileEdit(PostFileEdit {
                    context,
                    file_paths: edited_filepaths.into_iter().map(PathBuf::from).collect(),
                    dirty_files: dirty,
                    transcript_source,
                    tool_use_id,
                })
            }
            PiHookEvent::BeforeCommand => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id_str,
            }),
            PiHookEvent::AfterCommand => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id_str,
                transcript_source,
            }),
        };

        Ok(vec![event])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/")).join(name)
    }

    #[test]
    fn test_pi_before_edit() {
        let session_path = fixture_path("pi-session-simple.jsonl");
        let input = json!({
            "hook_event_name": "before_edit",
            "session_id": "pi-sess-123",
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
        let events = PiPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "pi");
                assert_eq!(e.context.session_id, "pi-sess-123");
                assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
                assert_eq!(e.context.cwd, PathBuf::from("/tmp/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/tmp/project/src/main.rs")]
                );
                assert!(e.dirty_files.is_some());
                let metadata = &e.context.metadata;
                assert_eq!(metadata.get("tool_name").map(String::as_str), Some("edit"));
                assert_eq!(
                    metadata.get("tool_name_raw").map(String::as_str),
                    Some("edit")
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_pi_after_edit() {
        let session_path = fixture_path("pi-session-simple.jsonl");
        let input = json!({
            "hook_event_name": "after_edit",
            "session_id": "pi-sess-456",
            "session_path": session_path,
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "edit",
            "edited_filepaths": ["/tmp/project/src/main.rs"]
        })
        .to_string();
        let events = PiPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "pi");
                assert_eq!(e.context.agent_id.model, "claude-sonnet-4-5");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/tmp/project/src/main.rs")]
                );
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::PiJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_pi_before_command() {
        let input = json!({
            "hook_event_name": "before_command",
            "session_id": "pi-sess-789",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "bash",
            "tool_use_id": "tu-abc123"
        })
        .to_string();
        let events = PiPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "pi");
                assert_eq!(e.context.session_id, "pi-sess-789");
                assert_eq!(e.tool_use_id, "tu-abc123");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_pi_after_command() {
        let input = json!({
            "hook_event_name": "after_command",
            "session_id": "pi-sess-012",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "bash",
            "tool_use_id": "tu-def456"
        })
        .to_string();
        let events = PiPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "pi");
                assert_eq!(e.tool_use_id, "tu-def456");
                assert!(matches!(
                    e.transcript_source,
                    Some(TranscriptSource {
                        format: TranscriptFormat::PiJsonl,
                        ..
                    })
                ));
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_pi_strips_provider_prefix() {
        assert_eq!(
            PiPreset::strip_provider_prefix("anthropic/claude-opus-4-5"),
            "claude-opus-4-5"
        );
        assert_eq!(
            PiPreset::strip_provider_prefix("claude-opus-4-5"),
            "claude-opus-4-5"
        );
        assert_eq!(PiPreset::strip_provider_prefix("gpt-5"), "gpt-5");
    }

    #[test]
    fn test_pi_rejects_unknown_tool() {
        let input = json!({
            "hook_event_name": "after_edit",
            "session_id": "pi-sess-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "unknown_tool",
            "edited_filepaths": ["/tmp/project/src/main.rs"]
        })
        .to_string();
        let result = PiPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unsupported Pi tool_name")
        );
    }

    #[test]
    fn test_pi_rejects_bash_with_edit_event() {
        let input = json!({
            "hook_event_name": "after_edit",
            "session_id": "pi-sess-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "bash",
            "edited_filepaths": ["/tmp/project/src/main.rs"]
        })
        .to_string();
        let result = PiPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("before_edit/after_edit events cannot be used with bash")
        );
    }

    #[test]
    fn test_pi_rejects_edit_with_command_event() {
        let input = json!({
            "hook_event_name": "before_command",
            "session_id": "pi-sess-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "edit"
        })
        .to_string();
        let result = PiPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("before_command/after_command events require a bash tool")
        );
    }

    #[test]
    fn test_pi_before_edit_requires_filepaths() {
        let input = json!({
            "hook_event_name": "before_edit",
            "session_id": "pi-sess-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "edit"
        })
        .to_string();
        let result = PiPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("non-empty will_edit_filepaths")
        );
    }

    #[test]
    fn test_pi_after_edit_requires_filepaths() {
        let input = json!({
            "hook_event_name": "after_edit",
            "session_id": "pi-sess-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "edit"
        })
        .to_string();
        let result = PiPreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("non-empty edited_filepaths")
        );
    }

    #[test]
    fn test_pi_default_tool_use_id() {
        let input = json!({
            "hook_event_name": "before_command",
            "session_id": "pi-sess-123",
            "session_path": fixture_path("pi-session-simple.jsonl"),
            "cwd": "/tmp/project",
            "model": "anthropic/claude-sonnet-4-5",
            "tool_name": "bash"
        })
        .to_string();
        let events = PiPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }
}
