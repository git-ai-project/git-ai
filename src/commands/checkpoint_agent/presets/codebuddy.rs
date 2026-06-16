//! CodeBuddy CN preset.
//!
//! CodeBuddy CN (Tencent) is Claude Code-compatible at the hook protocol layer
//! but diverges in three notable ways:
//!   1. `tool_input.filePath` is camelCase (Claude uses `file_path`).
//!   2. `cwd` is always `"/"` due to a known CodeBuddy bug — repo discovery
//!      relies on the absolute paths in `tool_input.filePath` instead.
//!   3. Identifying signals: `client: "CodeBuddyIDE"` and a transcript path
//!      under `CodeBuddyExtension`. CodeBuddy supports many models (e.g.
//!      `glm-5.0-turbo`, Claude variants, Hunyuan); the model field is
//!      passed through verbatim.
//!
//! Tool naming for file edits mirrors Claude (`Write`/`Edit`/`MultiEdit`),
//! so we reuse `bash_tool::Agent::Claude` for tool classification of edits.
//!
//! **Bash tool calls are intentionally not handled.** CodeBuddy's `cwd: "/"`
//! bug means `discover_repository_in_path` would either fail or — worse —
//! find an unrelated parent repo. Until CodeBuddy fixes the bug or sends
//! `tool_input.cwd`, we skip bash hooks entirely (no checkpoint emitted).
//!
//! Transcript reading for the directory-based `index.json + messages/` layout
//! is intentionally deferred — `transcript_source` is set to `None` so the
//! orchestrator does not try to feed a directory to a JSONL reader. Hook
//! attribution still works without it.

use super::parse;
use super::{AgentPreset, ParsedHookEvent, PostFileEdit, PreFileEdit, PresetContext};
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct CodeBuddyPreset;

impl CodeBuddyPreset {
    /// Extract the absolute file path from `tool_input.filePath`. CodeBuddy CN
    /// uses camelCase, distinct from Claude's `file_path`. The path is always
    /// absolute when present.
    fn file_path_from_tool_input(data: &Value) -> Vec<PathBuf> {
        let Some(tool_input) = data.get("tool_input") else {
            return vec![];
        };
        match tool_input.get("filePath").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => vec![PathBuf::from(p)],
            _ => vec![],
        }
    }

    /// Derive a session id from the transcript path when no explicit `session_id`
    /// field is present. CodeBuddy CN's transcript files are always named
    /// `index.json` and live under `.../<session-uuid>/index.json`, so the file
    /// stem ("index") is useless — use the parent directory name instead.
    fn session_id_from_transcript_path(transcript_path: &str) -> String {
        std::path::Path::new(transcript_path)
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string())
    }
}

impl AgentPreset for CodeBuddyPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let data: Value = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let transcript_path = parse::required_str(&data, "transcript_path")?;
        let cwd = parse::optional_str(&data, "cwd").unwrap_or("/");

        let session_id = parse::optional_str(&data, "session_id")
            .map(str::to_string)
            .unwrap_or_else(|| Self::session_id_from_transcript_path(transcript_path));

        let model = parse::optional_str(&data, "model")
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown")
            .to_string();

        let tool_name = parse::optional_str_multi(&data, &["tool_name", "toolName"]);
        let hook_event = parse::optional_str_multi(&data, &["hook_event_name", "hookEventName"]);

        // CodeBuddy mirrors Claude's tool taxonomy for file edits. Bash hooks
        // are explicitly skipped — see the module-level comment above.
        let tool_class = tool_name
            .map(|n| bash_tool::classify_tool(Agent::Claude, n))
            .unwrap_or(ToolClass::Skip);
        if tool_class == ToolClass::Bash {
            return Err(GitAiError::PresetError(
                "CodeBuddy bash hooks are not supported (cwd: \"/\" makes repo discovery unreliable)"
                    .to_string(),
            ));
        }

        let mut metadata =
            HashMap::from([("transcript_path".to_string(), transcript_path.to_string())]);
        if let Some(client) = parse::optional_str(&data, "client") {
            metadata.insert("client".to_string(), client.to_string());
        }
        if let Some(version) = parse::optional_str(&data, "version") {
            metadata.insert("client_version".to_string(), version.to_string());
        }
        if let Some(generation_id) = parse::optional_str(&data, "generation_id") {
            metadata.insert("generation_id".to_string(), generation_id.to_string());
        }

        let context = PresetContext {
            agent_id: AgentId {
                tool: "codebuddy".to_string(),
                id: session_id.clone(),
                model,
            },
            session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(cwd),
            metadata,
        };

        // Transcript reader for the directory-based index.json+messages/ layout
        // is not yet implemented. Set transcript_source to None — pointing the
        // existing JSONL reader at index.json (a JSON object, not JSONL) would
        // produce noisy parse errors in daemon logs without recovering any
        // events. Hook-based attribution works without it; a dedicated reader
        // can land in a follow-up.
        let transcript_source = None;

        let event = match hook_event {
            Some("PreToolUse") => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths: Self::file_path_from_tool_input(&data),
                dirty_files: None,
            }),
            _ => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths: Self::file_path_from_tool_input(&data),
                dirty_files: None,
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

    fn make_hook_input(event: &str, tool: &str) -> String {
        json!({
            "session_id": "sess-cb-1",
            "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/u1/CodeBuddyIDE/u2/history/u3/sess-cb-1/index.json",
            "cwd": "/",
            "hook_event_name": event,
            "tool_name": tool,
            "tool_input": {"filePath": "/private/tmp/git-ai-demo/app.py", "new_str": "print('hi')"},
            "generation_id": "gen-1",
            "model": "glm-5.0-turbo",
            "client": "CodeBuddyIDE",
            "version": "4.7.0"
        })
        .to_string()
    }

    #[test]
    fn test_codebuddy_post_file_edit_basic() {
        let input = make_hook_input("PostToolUse", "Edit");
        let events = CodeBuddyPreset.parse(&input, "t_test123456789a").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "codebuddy");
                assert_eq!(e.context.agent_id.id, "sess-cb-1");
                assert_eq!(e.context.session_id, "sess-cb-1");
                assert_eq!(e.context.trace_id, "t_test123456789a");
                assert_eq!(e.context.cwd, PathBuf::from("/"));
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/private/tmp/git-ai-demo/app.py")]
                );
                assert!(e.transcript_source.is_none());
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_codebuddy_uses_filepath_for_repo_discovery_when_cwd_is_root() {
        // When cwd is "/" (the CodeBuddy bug), the file_paths must still carry
        // the file path verbatim from `tool_input.filePath` so the orchestrator's
        // repo discovery (which uses file_paths[0], not cwd) succeeds. We don't
        // assert `is_absolute()` here because that's platform-dependent (Windows
        // considers a Unix-style `/private/tmp/...` non-absolute since it lacks
        // a drive letter), and CodeBuddy CN's hook payloads are produced by
        // macOS/Linux clients — what we care about is byte-for-byte preservation.
        let input = make_hook_input("PostToolUse", "Write");
        let events = CodeBuddyPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.cwd, PathBuf::from("/"));
                assert_eq!(e.file_paths.len(), 1);
                assert_eq!(
                    e.file_paths[0],
                    PathBuf::from("/private/tmp/git-ai-demo/app.py")
                );
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_codebuddy_extracts_model_from_hook_data() {
        let input = make_hook_input("PostToolUse", "Edit");
        let events = CodeBuddyPreset.parse(&input, "t_test123456789a").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "glm-5.0-turbo");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_codebuddy_default_model_when_missing() {
        let input = json!({
            "session_id": "sess-cb-2",
            "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/u1/CodeBuddyIDE/u2/history/u3/sess-cb-2/index.json",
            "cwd": "/",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"filePath": "/tmp/proj/file.rs"}
        })
        .to_string();
        let events = CodeBuddyPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.model, "unknown");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_codebuddy_pre_file_edit() {
        let input = make_hook_input("PreToolUse", "Edit");
        let events = CodeBuddyPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "codebuddy");
                assert_eq!(
                    e.file_paths,
                    vec![PathBuf::from("/private/tmp/git-ai-demo/app.py")]
                );
                assert!(e.dirty_files.is_none());
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_codebuddy_pre_bash_call_is_rejected() {
        // CodeBuddy bash hooks cannot be safely processed because cwd:"/"
        // makes repo discovery unreliable. The preset must explicitly reject
        // bash payloads rather than silently mis-attribute to the wrong repo.
        let input = json!({
            "session_id": "sess-cb-bash",
            "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/u1/CodeBuddyIDE/u2/history/u3/sess-cb-bash/index.json",
            "cwd": "/",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": "tu-bash-1",
            "tool_input": {"command": "ls"},
            "model": "glm-5.0-turbo",
            "client": "CodeBuddyIDE"
        })
        .to_string();
        let result = CodeBuddyPreset.parse(&input, "t_test");
        assert!(result.is_err());
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(
                    msg.contains("bash hooks are not supported"),
                    "expected bash rejection message, got: {}",
                    msg
                );
            }
            _ => panic!("Expected PresetError"),
        }
    }

    #[test]
    fn test_codebuddy_post_bash_call_is_rejected() {
        let input = json!({
            "session_id": "sess-cb-bash2",
            "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/u1/CodeBuddyIDE/u2/history/u3/sess-cb-bash2/index.json",
            "cwd": "/",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "tu-bash-2",
            "tool_input": {"command": "ls"},
            "model": "glm-5.0-turbo",
            "client": "CodeBuddyIDE"
        })
        .to_string();
        let result = CodeBuddyPreset.parse(&input, "t_test");
        assert!(result.is_err());
    }

    #[test]
    fn test_codebuddy_metadata_includes_client_and_version() {
        let input = make_hook_input("PostToolUse", "Edit");
        let events = CodeBuddyPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(
                    e.context.metadata.get("client").map(String::as_str),
                    Some("CodeBuddyIDE")
                );
                assert_eq!(
                    e.context.metadata.get("client_version").map(String::as_str),
                    Some("4.7.0")
                );
                assert_eq!(
                    e.context.metadata.get("generation_id").map(String::as_str),
                    Some("gen-1")
                );
                assert!(e.context.metadata.contains_key("transcript_path"));
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_codebuddy_session_id_falls_back_to_transcript_stem() {
        // No explicit session_id; should derive from transcript_path's parent
        // directory name (CodeBuddy stores transcripts as <session-id>/index.json,
        // so the file stem "index" is useless and we use the parent dir).
        let input = json!({
            "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/x/CodeBuddyIDE/y/history/z/derived-id-123/index.json",
            "cwd": "/",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"filePath": "/tmp/file.rs"}
        })
        .to_string();
        let events = CodeBuddyPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.session_id, "derived-id-123");
                assert_eq!(e.context.agent_id.id, "derived-id-123");
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_codebuddy_invalid_json_errors() {
        let result = CodeBuddyPreset.parse("not valid json", "t_test");
        assert!(result.is_err());
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(msg.contains("Invalid JSON"), "msg was: {}", msg);
            }
            _ => panic!("Expected PresetError"),
        }
    }

    #[test]
    fn test_codebuddy_missing_transcript_path_errors() {
        let input = json!({
            "session_id": "x",
            "cwd": "/",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {"filePath": "/tmp/file.rs"}
        })
        .to_string();
        let result = CodeBuddyPreset.parse(&input, "t_test");
        assert!(result.is_err());
        match result {
            Err(GitAiError::PresetError(msg)) => {
                assert!(
                    msg.contains("transcript_path"),
                    "expected transcript_path in msg: {}",
                    msg
                );
            }
            _ => panic!("Expected PresetError for missing transcript_path"),
        }
    }

    #[test]
    fn test_codebuddy_missing_tool_input_filepath_yields_empty_paths() {
        // Hook event with no filePath should still parse but produce empty
        // file_paths — the orchestrator will then no-op gracefully (no repo
        // discovery target, no checkpoint emitted).
        let input = json!({
            "session_id": "sess",
            "transcript_path": "/Users/u/Library/Application Support/CodeBuddyExtension/Data/x/CodeBuddyIDE/y/history/z/sess/index.json",
            "cwd": "/",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {}
        })
        .to_string();
        let events = CodeBuddyPreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert!(e.file_paths.is_empty());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }
}
