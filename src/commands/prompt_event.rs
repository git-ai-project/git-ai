//! Prompt event tracking command.
//!
//! Emits PromptEvent metrics for individual events within a prompt session
//! (human messages, AI messages, tool calls, etc.). Only works in async_mode
//! with daemon running.

use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::config;
use crate::error::GitAiError;
use crate::metrics::pos_encoded::PosEncoded;
use crate::metrics::{EventAttributes, MetricEvent, PromptEventValues};
use crate::utils::debug_log;
use sha2::{Digest, Sha256};
use std::io::Read;

/// PromptEvent kind enum values (serialized as strings).
pub mod prompt_event_kind {
    pub const HUMAN_MESSAGE: &str = "HumanMessage";
    pub const AI_MESSAGE: &str = "AiMessage";
    pub const THINKING_MESSAGE: &str = "ThinkingMessage";
    pub const TOOL_CALL: &str = "ToolCall";
    pub const FILE_WRITE: &str = "FileWrite";
}

/// A flattened event extracted from a transcript for PromptEvent emission.
#[derive(Debug, Clone)]
struct TranscriptEvent {
    kind: String,
    /// Content used for stable ID generation.
    content_hash_input: String,
    /// Timestamp of the event, if available.
    #[allow(dead_code)]
    timestamp: Option<String>,
}

/// State file for tracking emitted events per session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct PromptEventState {
    /// Number of transcript lines previously processed.
    last_line_count: usize,
    /// Event IDs already emitted (in order).
    emitted_event_ids: Vec<String>,
}

impl PromptEventState {
    fn state_dir() -> std::path::PathBuf {
        crate::mdm::utils::home_dir()
            .join(".git-ai")
            .join("internal")
            .join("prompt-events")
    }

    fn load(session_id: &str) -> Self {
        let path = Self::state_dir().join(format!("{}.json", session_id));
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    fn save(&self, session_id: &str) {
        let dir = Self::state_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join(format!("{}.json", session_id));
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(&path, json);
        }
    }
}

/// Compute a content-based stable event ID prefixed with the prompt_id.
fn compute_event_id(prompt_id: &str, kind: &str, content_hash_input: &str, index: usize) -> String {
    let combined = format!("{}:{}:{}:{}", prompt_id, kind, content_hash_input, index);
    let mut hasher = Sha256::new();
    hasher.update(combined.as_bytes());
    let result = hasher.finalize();
    let hash = format!("{:x}", result);
    format!("{}:{}", prompt_id, &hash[..12])
}

/// Parse a Claude Code JSONL transcript into a flat list of TranscriptEvents.
fn parse_claude_transcript(transcript_path: &str) -> Result<Vec<TranscriptEvent>, GitAiError> {
    let content = std::fs::read_to_string(transcript_path).map_err(GitAiError::IoError)?;
    let mut events = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let timestamp = entry["timestamp"].as_str().map(|s| s.to_string());

        match entry["type"].as_str() {
            Some("user") => {
                // User message - extract text content
                let text = if let Some(s) = entry["message"]["content"].as_str() {
                    s.to_string()
                } else if let Some(arr) = entry["message"]["content"].as_array() {
                    arr.iter()
                        .filter_map(|item| {
                            if item["type"].as_str() == Some("text") {
                                item["text"].as_str().map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    String::new()
                };
                if !text.trim().is_empty() {
                    events.push(TranscriptEvent {
                        kind: prompt_event_kind::HUMAN_MESSAGE.to_string(),
                        content_hash_input: truncate_for_hash(&text),
                        timestamp,
                    });
                }
            }
            Some("assistant") => {
                if let Some(content_array) = entry["message"]["content"].as_array() {
                    for item in content_array {
                        match item["type"].as_str() {
                            Some("thinking") => {
                                if let Some(thinking) = item["thinking"].as_str()
                                    && !thinking.trim().is_empty()
                                {
                                    events.push(TranscriptEvent {
                                        kind: prompt_event_kind::THINKING_MESSAGE.to_string(),
                                        content_hash_input: truncate_for_hash(thinking),
                                        timestamp: timestamp.clone(),
                                    });
                                }
                            }
                            Some("text") => {
                                if let Some(text) = item["text"].as_str()
                                    && !text.trim().is_empty()
                                {
                                    events.push(TranscriptEvent {
                                        kind: prompt_event_kind::AI_MESSAGE.to_string(),
                                        content_hash_input: truncate_for_hash(text),
                                        timestamp: timestamp.clone(),
                                    });
                                }
                            }
                            Some("tool_use") => {
                                let tool_name =
                                    item["name"].as_str().unwrap_or("unknown").to_string();
                                let tool_id = item["id"].as_str().unwrap_or("").to_string();
                                let is_file_write =
                                    matches!(tool_name.as_str(), "Write" | "Edit" | "MultiEdit");
                                events.push(TranscriptEvent {
                                    kind: if is_file_write {
                                        prompt_event_kind::FILE_WRITE.to_string()
                                    } else {
                                        prompt_event_kind::TOOL_CALL.to_string()
                                    },
                                    content_hash_input: format!("{}:{}", tool_name, tool_id),
                                    timestamp: timestamp.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {} // Skip tool_result and other types
        }
    }

    Ok(events)
}

/// Truncate content for hashing to keep IDs stable even if content grows.
fn truncate_for_hash(s: &str) -> String {
    if s.len() > 256 {
        s[..256].to_string()
    } else {
        s.to_string()
    }
}

/// Process prompt events for a Claude Code session.
fn process_claude_prompt_events(hook_input: &str) -> Result<(), GitAiError> {
    let hook_data: serde_json::Value = serde_json::from_str(hook_input)
        .map_err(|e| GitAiError::Generic(format!("Invalid hook JSON: {}", e)))?;

    let transcript_path = hook_data["transcript_path"]
        .as_str()
        .ok_or_else(|| GitAiError::Generic("Missing transcript_path".to_string()))?;

    // Extract session_id from hook input or derive from transcript path
    let session_id = hook_data["session_id"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::path::Path::new(transcript_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

    let prompt_id = generate_short_hash(&session_id, "claude");

    // Parse transcript events
    let all_events = match parse_claude_transcript(transcript_path) {
        Ok(events) => events,
        Err(e) => {
            debug_log(&format!("prompt-event: failed to parse transcript: {}", e));
            // Still try to emit a single event for the current hook trigger
            emit_single_event_from_hook(&hook_data, &prompt_id, &session_id)?;
            return Ok(());
        }
    };

    // Load state
    let mut state = PromptEventState::load(&session_id);

    // Detect rollback: if transcript is shorter than what we've seen
    let rollback_detected = all_events.len() < state.last_line_count;
    if rollback_detected {
        debug_log(&format!(
            "prompt-event: rollback detected for session {}: {} events now vs {} previously",
            session_id,
            all_events.len(),
            state.last_line_count
        ));
        // Reset state - re-process from scratch but don't re-emit already-known events
        // We keep emitted_event_ids to avoid duplicates
    }

    // Compute event IDs for all events
    let event_ids: Vec<String> = all_events
        .iter()
        .enumerate()
        .map(|(i, evt)| compute_event_id(&prompt_id, &evt.kind, &evt.content_hash_input, i))
        .collect();

    // Find new events (not yet emitted)
    let mut new_events = Vec::new();
    for (i, eid) in event_ids.iter().enumerate() {
        if !state.emitted_event_ids.contains(eid) {
            // Determine parent
            let (parent_id, parent_estimated) = if i > 0 {
                (Some(event_ids[i - 1].clone()), false)
            } else {
                (None, false)
            };
            new_events.push((i, eid.clone(), parent_id, parent_estimated));
        }
    }

    if new_events.is_empty() {
        return Ok(());
    }

    // Build and emit metric events
    let attrs = build_prompt_event_attrs(&prompt_id, &session_id);
    let mut metric_events = Vec::new();

    for (i, eid, parent_id, parent_estimated) in &new_events {
        let evt = &all_events[*i];
        let mut values = PromptEventValues::new().kind(&evt.kind).event_id(eid);

        match parent_id {
            Some(pid) => {
                values = values.parent_id(pid);
            }
            None => {
                values = values.parent_id_null();
            }
        }
        values = values.parent_id_estimated(*parent_estimated);

        let event = MetricEvent::new(&values, attrs.to_sparse());
        metric_events.push(event);
    }

    // Submit via daemon telemetry
    if !metric_events.is_empty() {
        crate::observability::log_metrics(metric_events);
    }

    // Update state
    state.last_line_count = all_events.len();
    for (_, eid, _, _) in new_events {
        if !state.emitted_event_ids.contains(&eid) {
            state.emitted_event_ids.push(eid);
        }
    }
    state.save(&session_id);

    Ok(())
}

/// Emit a single event directly from hook data when transcript parsing fails.
fn emit_single_event_from_hook(
    hook_data: &serde_json::Value,
    prompt_id: &str,
    session_id: &str,
) -> Result<(), GitAiError> {
    let hook_event_name = hook_data["hook_event_name"].as_str().unwrap_or("unknown");

    let kind = match hook_event_name {
        "UserPromptSubmit" => prompt_event_kind::HUMAN_MESSAGE,
        "PostToolUse" => {
            let tool_name = hook_data["tool_name"].as_str().unwrap_or("unknown");
            match tool_name {
                "Write" | "Edit" | "MultiEdit" => prompt_event_kind::FILE_WRITE,
                _ => prompt_event_kind::TOOL_CALL,
            }
        }
        "Stop" => prompt_event_kind::AI_MESSAGE,
        _ => prompt_event_kind::TOOL_CALL,
    };

    // Build a content hash from whatever we have
    let content_hash_input = match hook_event_name {
        "UserPromptSubmit" => hook_data["prompt"].as_str().unwrap_or("").to_string(),
        "PostToolUse" => {
            let tool_name = hook_data["tool_name"].as_str().unwrap_or("unknown");
            let tool_id = hook_data["tool_use_id"].as_str().unwrap_or("");
            format!("{}:{}", tool_name, tool_id)
        }
        _ => format!("hook:{}", hook_event_name),
    };

    let event_id = compute_event_id(prompt_id, kind, &content_hash_input, 0);

    // Try to get parent from state
    let state = PromptEventState::load(session_id);
    let (parent_id, parent_estimated) = if let Some(last) = state.emitted_event_ids.last() {
        (Some(last.clone()), true) // Estimated since transcript failed
    } else {
        (None, false)
    };

    let mut values = PromptEventValues::new().kind(kind).event_id(&event_id);
    match parent_id {
        Some(pid) => {
            values = values.parent_id(pid);
        }
        None => {
            values = values.parent_id_null();
        }
    }
    values = values.parent_id_estimated(parent_estimated);

    let attrs = build_prompt_event_attrs(prompt_id, session_id);
    let metric_event = MetricEvent::new(&values, attrs.to_sparse());
    crate::observability::log_metrics(vec![metric_event]);

    // Update state
    let mut state = state;
    state.emitted_event_ids.push(event_id);
    state.save(session_id);

    Ok(())
}

/// Build common EventAttributes for prompt events.
fn build_prompt_event_attrs(prompt_id: &str, session_id: &str) -> EventAttributes {
    EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .tool("claude")
        .prompt_id(prompt_id)
        .external_prompt_id(session_id)
}

/// Process prompt events for an OpenCode session.
fn process_opencode_prompt_events(hook_input: &str) -> Result<(), GitAiError> {
    let hook_data: serde_json::Value = serde_json::from_str(hook_input)
        .map_err(|e| GitAiError::Generic(format!("Invalid hook JSON: {}", e)))?;

    let session_id = hook_data["session_id"]
        .as_str()
        .ok_or_else(|| GitAiError::Generic("Missing session_id".to_string()))?;

    let prompt_id = generate_short_hash(session_id, "opencode");

    let hook_event_name = hook_data["hook_event_name"].as_str().unwrap_or("unknown");

    let kind = match hook_event_name {
        "PreToolUse" => prompt_event_kind::HUMAN_MESSAGE,
        "PostToolUse" => {
            let tool_name = hook_data["tool_input"]
                .get("toolName")
                .or_else(|| hook_data.get("tool_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match tool_name {
                "edit" | "write" | "patch" | "multiedit" => prompt_event_kind::FILE_WRITE,
                _ => prompt_event_kind::TOOL_CALL,
            }
        }
        _ => prompt_event_kind::TOOL_CALL,
    };

    // Build content hash from hook data
    let content_hash_input = format!(
        "{}:{}:{}",
        hook_event_name,
        hook_data["tool_input"],
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let event_id = compute_event_id(&prompt_id, kind, &content_hash_input, 0);

    // Get parent from state
    let state = PromptEventState::load(session_id);
    let (parent_id, parent_estimated) = if let Some(last) = state.emitted_event_ids.last() {
        // For OpenCode we don't have transcript access, so parent is always estimated
        (Some(last.clone()), true)
    } else {
        (None, false)
    };

    let mut values = PromptEventValues::new().kind(kind).event_id(&event_id);
    match parent_id {
        Some(pid) => {
            values = values.parent_id(pid);
        }
        None => {
            values = values.parent_id_null();
        }
    }
    values = values.parent_id_estimated(parent_estimated);

    let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .tool("opencode")
        .prompt_id(&prompt_id)
        .external_prompt_id(session_id);

    let metric_event = MetricEvent::new(&values, attrs.to_sparse());
    crate::observability::log_metrics(vec![metric_event]);

    // Update state
    let mut state = state;
    state.emitted_event_ids.push(event_id);
    state.save(session_id);

    Ok(())
}

/// Entry point for the `prompt-event` command.
pub fn handle_prompt_event(args: &[String]) {
    // Only works in async_mode with daemon running
    if !config::Config::get().feature_flags().async_mode {
        debug_log("prompt-event: skipping, async_mode not enabled");
        return;
    }

    if !crate::daemon::telemetry_handle::daemon_telemetry_available() {
        debug_log("prompt-event: skipping, daemon telemetry not available");
        return;
    }

    if args.is_empty() {
        eprintln!("Usage: git-ai prompt-event <agent> --hook-input stdin");
        std::process::exit(1);
    }

    let agent = &args[0];
    let mut hook_input = None;

    // Parse --hook-input
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--hook-input" && i + 1 < args.len() {
            let source = &args[i + 1];
            if source == "stdin" {
                let mut buffer = String::new();
                if std::io::stdin().read_to_string(&mut buffer).is_ok() {
                    hook_input = Some(buffer);
                }
            } else {
                hook_input = Some(source.clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }

    let hook_input = match hook_input {
        Some(input) if !input.trim().is_empty() => input,
        _ => {
            debug_log("prompt-event: no hook input provided");
            return;
        }
    };

    let result = match agent.as_str() {
        "claude" => process_claude_prompt_events(&hook_input),
        "opencode" => process_opencode_prompt_events(&hook_input),
        other => {
            debug_log(&format!("prompt-event: unsupported agent: {}", other));
            return;
        }
    };

    if let Err(e) = result {
        debug_log(&format!("prompt-event: error: {}", e));
        // Don't exit with error - prompt events are best-effort
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_event_id_is_stable() {
        let id1 = compute_event_id("prompt1", "HumanMessage", "hello", 0);
        let id2 = compute_event_id("prompt1", "HumanMessage", "hello", 0);
        assert_eq!(id1, id2);
        assert!(id1.starts_with("prompt1:"));
    }

    #[test]
    fn test_compute_event_id_different_content() {
        let id1 = compute_event_id("prompt1", "HumanMessage", "hello", 0);
        let id2 = compute_event_id("prompt1", "HumanMessage", "world", 0);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_compute_event_id_different_prompt() {
        let id1 = compute_event_id("prompt1", "HumanMessage", "hello", 0);
        let id2 = compute_event_id("prompt2", "HumanMessage", "hello", 0);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_compute_event_id_different_index() {
        let id1 = compute_event_id("prompt1", "HumanMessage", "hello", 0);
        let id2 = compute_event_id("prompt1", "HumanMessage", "hello", 1);
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_truncate_for_hash() {
        let short = "hello";
        assert_eq!(truncate_for_hash(short), "hello");

        let long = "a".repeat(500);
        let truncated = truncate_for_hash(&long);
        assert_eq!(truncated.len(), 256);
    }

    #[test]
    fn test_parse_claude_transcript_basic() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("transcript.jsonl");

        let content = r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello"}}
{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"model":"claude-3","content":[{"type":"thinking","thinking":"Let me think"},{"type":"text","text":"Hi there!"},{"type":"tool_use","id":"toolu_123","name":"Write","input":{"file_path":"test.rs"}}]}}"#;
        std::fs::write(&path, content).unwrap();

        let events = parse_claude_transcript(path.to_str().unwrap()).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].kind, "HumanMessage");
        assert_eq!(events[1].kind, "ThinkingMessage");
        assert_eq!(events[2].kind, "AiMessage");
        assert_eq!(events[3].kind, "FileWrite");
    }

    #[test]
    fn test_parse_claude_transcript_tool_use_kinds() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("transcript.jsonl");

        let content = r#"{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"model":"claude-3","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"test.rs"}},{"type":"tool_use","id":"toolu_2","name":"Edit","input":{"file_path":"test.rs"}},{"type":"tool_use","id":"toolu_3","name":"Bash","input":{"command":"ls"}}]}}"#;
        std::fs::write(&path, content).unwrap();

        let events = parse_claude_transcript(path.to_str().unwrap()).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "ToolCall"); // Read
        assert_eq!(events[1].kind, "FileWrite"); // Edit
        assert_eq!(events[2].kind, "ToolCall"); // Bash
    }

    #[test]
    fn test_prompt_event_state_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = PromptEventState {
            last_line_count: 5,
            emitted_event_ids: vec!["evt1".to_string(), "evt2".to_string()],
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: PromptEventState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.last_line_count, 5);
        assert_eq!(restored.emitted_event_ids.len(), 2);
        drop(dir);
    }

    #[test]
    fn test_prompt_event_values_builder() {
        let values = PromptEventValues::new()
            .kind("HumanMessage")
            .event_id("prompt1:abc123")
            .parent_id("prompt1:def456")
            .parent_id_estimated(false);

        assert_eq!(values.kind, Some(Some("HumanMessage".to_string())));
        assert_eq!(values.event_id, Some(Some("prompt1:abc123".to_string())));
        assert_eq!(values.parent_id, Some(Some("prompt1:def456".to_string())));
        assert_eq!(values.parent_id_estimated, Some(Some(false)));
    }

    #[test]
    fn test_prompt_event_values_null_parent() {
        let values = PromptEventValues::new()
            .kind("HumanMessage")
            .event_id("prompt1:abc123")
            .parent_id_null()
            .parent_id_estimated(false);

        assert_eq!(values.parent_id, Some(None));
    }

    #[test]
    fn test_prompt_event_values_to_sparse() {
        use crate::metrics::pos_encoded::PosEncoded;

        let values = PromptEventValues::new()
            .kind("ToolCall")
            .event_id("p1:abc")
            .parent_id("p1:def")
            .parent_id_estimated(true);

        let sparse = PosEncoded::to_sparse(&values);
        assert_eq!(
            sparse.get("0"),
            Some(&serde_json::Value::String("ToolCall".to_string()))
        );
        assert_eq!(
            sparse.get("1"),
            Some(&serde_json::Value::String("p1:abc".to_string()))
        );
        assert_eq!(
            sparse.get("2"),
            Some(&serde_json::Value::String("p1:def".to_string()))
        );
        assert_eq!(sparse.get("3"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn test_prompt_event_values_roundtrip() {
        use crate::metrics::pos_encoded::PosEncoded;

        let original = PromptEventValues::new()
            .kind("FileWrite")
            .event_id("p1:xyz")
            .parent_id_null()
            .parent_id_estimated(false);

        let sparse = PosEncoded::to_sparse(&original);
        let restored = <PromptEventValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.kind, Some(Some("FileWrite".to_string())));
        assert_eq!(restored.event_id, Some(Some("p1:xyz".to_string())));
        assert_eq!(restored.parent_id, Some(None));
        assert_eq!(restored.parent_id_estimated, Some(Some(false)));
    }

    #[test]
    fn test_prompt_event_metric_event_id() {
        use crate::metrics::types::{EventValues, MetricEventId};
        assert_eq!(
            <PromptEventValues as EventValues>::event_id(),
            MetricEventId::PromptEvent
        );
        assert_eq!(<PromptEventValues as EventValues>::event_id() as u16, 5);
    }
}
