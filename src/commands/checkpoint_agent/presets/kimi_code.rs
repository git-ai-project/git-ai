//! Kimi Code (Moonshot AI / kimi-cli) preset.
//!
//! Kimi Code's hook protocol delivers a single-line UTF-8 JSON payload on
//! stdin, with the following top-level fields confirmed against the
//! upstream source (`MoonshotAI/kimi-cli`, `src/kimi_cli/hooks/events.py`):
//!
//!   - `hook_event_name` — `"PreToolUse"`, `"PostToolUse"`, etc.
//!   - `session_id`      — opaque session identifier
//!   - `cwd`             — working directory at hook-fire time
//!   - `tool_name`       — present on `PreToolUse` / `PostToolUse`
//!   - `tool_input`      — present on `PreToolUse` / `PostToolUse`
//!   - `tool_call_id`    — present on `PreToolUse` / `PostToolUse`
//!
//! Notably **absent** from the payload (verified against upstream source):
//!   - `model`            — kimi-cli does not expose the active model
//!   - `transcript_path`  — sessions live on disk under
//!     `$KIMI_SHARE_DIR/sessions/<work-dir-hash>/<session_id>/context.jsonl`
//!     (default base `~/.kimi/`), but the hook payload does not include
//!     that path.
//!
//! Tool naming differs from Claude:
//!   - `WriteFile`       → file edit (Pydantic field `path`, not `file_path`)
//!   - `StrReplaceFile`  → file edit (Pydantic field `path`)
//!   - `Shell`           → bash    (`tool_input.command`)
//!
//! `parse::file_paths_from_tool_input` already accepts `path` as one of the
//! field names it probes, so it works for Kimi without a custom extractor.
//!
//! **Events not handled:** kimi-cli also fires `PostToolUseFailure`,
//! `UserPromptSubmit`, `Stop`, `StopFailure`, `SessionStart`, `SessionEnd`,
//! `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact`,
//! `Notification`. The installer subscribes only to `PreToolUse` and
//! `PostToolUse` (see `mdm/agents/kimi_code.rs::KIMI_HOOK_EVENTS`), so
//! these other events should not normally reach this preset. We
//! defensively reject any event other than `PreToolUse`/`PostToolUse`
//! with an explicit `PresetError` — including `PostToolUseFailure`. A
//! failed bash command may still have mutated the workspace, so a future
//! enhancement could route `PostToolUseFailure` through the bash diff
//! flow; explicit rejection avoids silently mis-attributing partial state.
//!
//! Transcript reading for the directory-based `context.jsonl` format is not
//! yet wired into the shared `TranscriptFormat` enum — `transcript_source`
//! is therefore set to `None`. Hook-based attribution still works without
//! it; a dedicated reader can land in a follow-up PR.

use super::parse;
use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext,
};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct KimiCodePreset;

impl AgentPreset for KimiCodePreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: serde_json::Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let session_id = parse::required_str(&data, "session_id")?.to_string();
        let cwd = parse::required_str(&data, "cwd")?;

        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);
        // kimi-cli uses `tool_call_id` (per upstream source); accept the
        // Claude-style aliases too for forward compatibility. Default to
        // "unknown" rather than "bash" so file-edit events with a missing
        // id are not labelled with a bash-tool placeholder in logs.
        let tool_use_id = parse::str_or_default_multi(
            &data,
            &["tool_call_id", "tool_use_id", "toolUseId"],
            "unknown",
        );

        let tool_class = tool_name
            .map(|n| bash_tool::classify_tool(Agent::KimiCode, n))
            .unwrap_or(ToolClass::Skip);
        let is_bash = tool_class == ToolClass::Bash;
        let is_file_edit = tool_class == ToolClass::FileEdit;

        let context = PresetContext {
            agent_id: AgentId {
                tool: "kimi-code".to_string(),
                id: session_id.clone(),
                // kimi-cli does not surface the model in hook payloads. Default
                // to "unknown" rather than reading agent config files (no other
                // preset reads agent config; the model is properly recovered
                // by the transcript reader once one exists for Kimi).
                model: "unknown".to_string(),
            },
            external_session_id: session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata: HashMap::new(),
        };

        // Mirror Codex's strict event handling: explicit error on unknown
        // events so we never fabricate spurious file-edit checkpoints when
        // kimi-cli fires `Stop`, `SessionStart`, `SubagentStart`, etc.
        let event = match hook_event {
            Some("PreToolUse") => {
                if is_bash {
                    ParsedHookEvent::PreBashCall(PreBashCall {
                        context,
                        tool_use_id: tool_use_id.to_string(),
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PreFileEdit(PreFileEdit {
                        context,
                        file_paths: parse::file_paths_from_tool_input(&data, cwd),
                        dirty_files: None,
                        tool_use_id: Some(tool_use_id.to_string()),
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Kimi Code PreToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            Some("PostToolUse") => {
                if is_bash {
                    ParsedHookEvent::PostBashCall(PostBashCall {
                        context,
                        tool_use_id: tool_use_id.to_string(),
                        // Transcript reader for Kimi's context.jsonl format is
                        // not yet implemented; setting None avoids feeding an
                        // unsupported format to the existing JSONL readers.
                        transcript_source: None,
                    })
                } else if is_file_edit {
                    ParsedHookEvent::PostFileEdit(PostFileEdit {
                        context,
                        file_paths: parse::file_paths_from_tool_input(&data, cwd),
                        dirty_files: None,
                        transcript_source: None,
                        tool_use_id: Some(tool_use_id.to_string()),
                    })
                } else {
                    return Err(GitAiError::PresetError(format!(
                        "Skipping Kimi Code PostToolUse for unsupported tool {}",
                        tool_name.unwrap_or("unknown")
                    )));
                }
            }
            _ => {
                return Err(GitAiError::PresetError(format!(
                    "Unsupported Kimi Code hook_event_name: {}",
                    hook_event.unwrap_or("<missing>")
                )));
            }
        };

        Ok(vec![event])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;

    fn make_hook_input(event: &str, tool: &str, tool_input: serde_json::Value) -> String {
        json!({
            "session_id": "kimi-sess-1",
            "cwd": "/home/user/project",
            "hook_event_name": event,
            "tool_name": tool,
            "tool_call_id": "tc-1",
            "tool_input": tool_input,
        })
        .to_string()
    }

    #[test]
    fn test_kimi_code_pre_file_edit_writefile() {
        // WriteFile uses `path`, not `file_path` — verified against upstream.
        let input = make_hook_input(
            "PreToolUse",
            "WriteFile",
            json!({"path": "src/main.rs", "content": "fn main() {}", "mode": "overwrite"}),
        );
        let events = KimiCodePreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "kimi-code");
                assert_eq!(e.context.agent_id.id, "kimi-sess-1");
                assert_eq!(e.context.agent_id.model, "unknown");
                assert_eq!(e.context.external_session_id, "kimi-sess-1");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_kimi_code_post_file_edit_writefile() {
        let input = make_hook_input(
            "PostToolUse",
            "WriteFile",
            json!({"path": "src/main.rs", "content": "fn main() {}", "mode": "overwrite"}),
        );
        let events = KimiCodePreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "kimi-code");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/src/main.rs")]
                );
                assert!(
                    e.transcript_source.is_none(),
                    "Transcript reader not yet implemented; should be None"
                );
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_kimi_code_post_file_edit_strreplacefile() {
        // StrReplaceFile also uses `path`.
        let input = make_hook_input(
            "PostToolUse",
            "StrReplaceFile",
            json!({"path": "/home/user/project/foo.py", "edit": {"old": "a", "new": "b"}}),
        );
        let events = KimiCodePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/home/user/project/foo.py")]
                );
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_kimi_code_pre_bash_call() {
        let input = make_hook_input("PreToolUse", "Shell", json!({"command": "ls -la"}));
        let events = KimiCodePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "kimi-code");
                assert_eq!(e.tool_use_id, "tc-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_kimi_code_post_bash_call() {
        let input = make_hook_input("PostToolUse", "Shell", json!({"command": "ls -la"}));
        let events = KimiCodePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "kimi-code");
                assert_eq!(e.tool_use_id, "tc-1");
                assert!(e.transcript_source.is_none());
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_kimi_code_unsupported_tool_pretooluse_errors() {
        // Tools like ReadFile, Grep, Glob, SearchWeb, FetchURL, LaborMarket
        // are documented but we don't checkpoint for them — verify they error
        // rather than silently fabricating a checkpoint.
        let input = make_hook_input(
            "PreToolUse",
            "ReadFile",
            json!({"path": "/home/user/project/foo.rs"}),
        );
        let result = KimiCodePreset.parse(&input, "t_test");
        assert!(result.is_err());
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(
                    msg.contains("PreToolUse for unsupported tool"),
                    "expected unsupported-tool message, got: {}",
                    msg
                );
            }
            _ => panic!("Expected PresetError"),
        }
    }

    #[test]
    fn test_kimi_code_unsupported_tool_posttooluse_errors() {
        let input = make_hook_input("PostToolUse", "Grep", json!({"pattern": "TODO"}));
        let result = KimiCodePreset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_kimi_code_unknown_event_errors() {
        // Lifecycle events like SessionStart/SessionEnd/Stop/SubagentStart
        // must not silently fall through to PostFileEdit.
        let input = json!({
            "session_id": "kimi-sess-1",
            "cwd": "/home/user/project",
            "hook_event_name": "SessionStart",
            "source": "startup",
        })
        .to_string();
        let result = KimiCodePreset.parse(&input, "t_test");
        assert!(result.is_err());
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(
                    msg.contains("Unsupported Kimi Code hook_event_name"),
                    "expected unknown-event message, got: {}",
                    msg
                );
            }
            _ => panic!("Expected PresetError"),
        }
    }

    #[test]
    fn test_kimi_code_missing_event_name_errors() {
        let input = json!({
            "session_id": "kimi-sess-1",
            "cwd": "/home/user/project",
        })
        .to_string();
        let result = KimiCodePreset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_kimi_code_invalid_json_errors() {
        let result = KimiCodePreset.parse("not valid json", "t_test");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid JSON in hook_input")
        );
    }

    #[test]
    fn test_kimi_code_missing_session_id_errors() {
        let input = json!({
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "WriteFile",
            "tool_input": {"path": "x"},
        })
        .to_string();
        let result = KimiCodePreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_id"));
    }

    #[test]
    fn test_kimi_code_missing_cwd_errors() {
        let input = json!({
            "session_id": "kimi-sess-1",
            "hook_event_name": "PostToolUse",
            "tool_name": "WriteFile",
            "tool_input": {"path": "x"},
        })
        .to_string();
        let result = KimiCodePreset.parse(&input, "t_test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cwd"));
    }

    #[test]
    fn test_kimi_code_tool_use_id_aliases() {
        // Accept Claude-style `tool_use_id` and camelCase `toolUseId` for
        // forward compatibility, even though kimi-cli currently uses
        // `tool_call_id`.
        let alias_input = json!({
            "session_id": "kimi-sess-1",
            "cwd": "/home/user/project",
            "hook_event_name": "PreToolUse",
            "tool_name": "Shell",
            "tool_use_id": "tu-claude-style",
            "tool_input": {"command": "ls"},
        })
        .to_string();
        let events = KimiCodePreset.parse(&alias_input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "tu-claude-style");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_kimi_code_post_tool_use_with_tool_output_field() {
        // PostToolUse from kimi-cli includes `tool_output: str` — verify
        // we tolerate it (it should be ignored by the parser).
        let input = json!({
            "session_id": "kimi-sess-1",
            "cwd": "/home/user/project",
            "hook_event_name": "PostToolUse",
            "tool_name": "WriteFile",
            "tool_call_id": "tc-9",
            "tool_input": {"path": "src/lib.rs", "content": "// hi", "mode": "overwrite"},
            "tool_output": "wrote 5 bytes",
        })
        .to_string();
        let events = KimiCodePreset.parse(&input, "t_test").unwrap();
        assert!(matches!(events[0], ParsedHookEvent::PostFileEdit(_)));
    }
}
