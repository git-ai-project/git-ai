//! Transcript readers: format-specific parsers that extract `AiTranscript`
//! and optional model info from agent transcript files.
//!
//! Each `read_*` function corresponds to one `TranscriptFormat` variant and
//! accepts a filesystem path (plus, for OpenCode formats, a `session_id`).
//! The top-level [`read_transcript`] function dispatches on `TranscriptSource`.

use crate::authorship::transcript::{AiTranscript, Message};
use crate::commands::checkpoint_agent::agent_presets::extract_plan_from_tool_use;
use crate::commands::checkpoint_agent::presets::{TranscriptFormat, TranscriptSource};
use crate::error::GitAiError;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Public dispatch
// ---------------------------------------------------------------------------

/// Read a transcript from the given source.
pub fn read_transcript(
    source: &TranscriptSource,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    match source {
        TranscriptSource::Path {
            path,
            format,
            session_id,
        } => read_from_path(path, *format, session_id.as_deref()),
        TranscriptSource::Inline(transcript) => Ok((transcript.clone(), None)),
    }
}

fn read_from_path(
    path: &Path,
    format: TranscriptFormat,
    session_id: Option<&str>,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    match format {
        TranscriptFormat::ClaudeJsonl => read_claude_jsonl(path),
        TranscriptFormat::GeminiJson => read_gemini_json(path),
        TranscriptFormat::WindsurfJsonl => read_windsurf_jsonl(path),
        TranscriptFormat::CodexJsonl => read_codex_jsonl(path),
        TranscriptFormat::CursorJsonl => read_cursor_jsonl(path),
        TranscriptFormat::DroidJsonl => read_droid_jsonl(path),
        TranscriptFormat::CopilotSessionJson => {
            read_copilot_session_json(path).map(|(t, m, _)| (t, m))
        }
        TranscriptFormat::CopilotEventStreamJsonl => {
            let content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;
            read_copilot_event_stream_jsonl(&content).map(|(t, m, _)| (t, m))
        }
        TranscriptFormat::AmpThreadJson => read_amp_thread_json(path).map(|(t, m, _)| (t, m)),
        TranscriptFormat::OpenCodeSqlite => {
            let sid = session_id.ok_or_else(|| {
                GitAiError::PresetError(
                    "session_id is required for OpenCodeSqlite transcript format".to_string(),
                )
            })?;
            read_opencode_sqlite(path, sid)
        }
        TranscriptFormat::OpenCodeLegacyJson => {
            let sid = session_id.ok_or_else(|| {
                GitAiError::PresetError(
                    "session_id is required for OpenCodeLegacyJson transcript format".to_string(),
                )
            })?;
            read_opencode_legacy_json(path, sid)
        }
        TranscriptFormat::PiJsonl => read_pi_jsonl(path),
    }
}

// ---------------------------------------------------------------------------
// Claude Code JSONL
// ---------------------------------------------------------------------------

/// Parse a Claude Code JSONL file into a transcript and extract model info.
pub fn read_claude_jsonl(path: &Path) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let jsonl_content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;
    let mut transcript = AiTranscript::new();
    let mut model = None;
    let mut plan_states = std::collections::HashMap::new();

    for line in jsonl_content.lines() {
        if !line.trim().is_empty() {
            // Parse the raw JSONL entry
            let raw_entry: serde_json::Value = serde_json::from_str(line)?;
            let timestamp = raw_entry["timestamp"].as_str().map(|s| s.to_string());

            // Extract model from assistant messages if we haven't found it yet
            if model.is_none()
                && raw_entry["type"].as_str() == Some("assistant")
                && let Some(model_str) = raw_entry["message"]["model"].as_str()
            {
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
                    } else if let Some(content_array) = raw_entry["message"]["content"].as_array() {
                        // Handle user messages with content array
                        for item in content_array {
                            // Skip tool_result items - those are system-generated responses, not human input
                            if item["type"].as_str() == Some("tool_result") {
                                continue;
                            }
                            // Handle text content blocks from actual user input
                            if item["type"].as_str() == Some("text")
                                && let Some(text) = item["text"].as_str()
                                && !text.trim().is_empty()
                            {
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
                                        && !text.trim().is_empty()
                                    {
                                        transcript.add_message(Message::Assistant {
                                            text: text.to_string(),
                                            timestamp: timestamp.clone(),
                                        });
                                    }
                                }
                                Some("thinking") => {
                                    if let Some(thinking) = item["thinking"].as_str()
                                        && !thinking.trim().is_empty()
                                    {
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
                                        // Check if this is a Write/Edit to a plan file
                                        if let Some(plan_text) = extract_plan_from_tool_use(
                                            name,
                                            &item["input"],
                                            &mut plan_states,
                                        ) {
                                            transcript.add_message(Message::Plan {
                                                text: plan_text,
                                                timestamp: timestamp.clone(),
                                            });
                                        } else {
                                            transcript.add_message(Message::ToolUse {
                                                name: name.to_string(),
                                                input: item["input"].clone(),
                                                timestamp: timestamp.clone(),
                                            });
                                        }
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

// ---------------------------------------------------------------------------
// Gemini JSON
// ---------------------------------------------------------------------------

/// Parse a Gemini JSON file into a transcript and extract model info.
pub fn read_gemini_json(path: &Path) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let json_content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;
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
                    && let Some(model_str) = message.get("model").and_then(|v| v.as_str())
                {
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

// ---------------------------------------------------------------------------
// Windsurf JSONL
// ---------------------------------------------------------------------------

/// Parse a Windsurf JSONL transcript file into a transcript.
/// Each line is a JSON object with a "type" field.
/// Model info is not present in the JSONL format -- always returns None.
/// (Model is instead provided via `model_name` in the hook payload.)
pub fn read_windsurf_jsonl(path: &Path) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;

    let mut transcript = AiTranscript::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines
        };

        let entry_type = match entry.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };

        let timestamp = entry
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Windsurf nests data under a key matching the type name,
        // e.g. {"type": "user_input", "user_input": {"user_response": "..."}}
        let inner = entry.get(entry_type);

        match entry_type {
            "user_input" => {
                if let Some(text) = inner
                    .and_then(|v| v.get("user_response"))
                    .and_then(|v| v.as_str())
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        transcript.add_message(Message::User {
                            text: trimmed.to_string(),
                            timestamp,
                        });
                    }
                }
            }
            "planner_response" => {
                if let Some(text) = inner
                    .and_then(|v| v.get("response"))
                    .and_then(|v| v.as_str())
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        transcript.add_message(Message::Assistant {
                            text: trimmed.to_string(),
                            timestamp,
                        });
                    }
                }
            }
            "code_action" => {
                if let Some(action) = inner {
                    let path_val = action
                        .get("path")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let new_content = action
                        .get("new_content")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);

                    transcript.add_message(Message::ToolUse {
                        name: "code_action".to_string(),
                        input: serde_json::json!({
                            "path": path_val,
                            "new_content": new_content,
                        }),
                        timestamp,
                    });
                }
            }
            "view_file" | "run_command" | "find" | "grep_search" | "list_directory"
            | "list_resources" => {
                // Map all tool-like actions to ToolUse
                let input = inner.cloned().unwrap_or(serde_json::json!({}));
                transcript.add_message(Message::ToolUse {
                    name: entry_type.to_string(),
                    input,
                    timestamp,
                });
            }
            _ => {
                // Skip truly unknown types silently
                continue;
            }
        }
    }

    // Model info is not present in Windsurf JSONL format
    Ok((transcript, None))
}

// ---------------------------------------------------------------------------
// Codex JSONL
// ---------------------------------------------------------------------------

/// Parse a Codex rollout JSONL file into a transcript and extract model info.
pub fn read_codex_jsonl(path: &Path) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let jsonl_content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;

    let mut parsed_lines: Vec<serde_json::Value> = Vec::new();
    for line in jsonl_content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)?;
        parsed_lines.push(value);
    }

    let mut transcript = AiTranscript::new();
    let mut model = None;

    for entry in &parsed_lines {
        let timestamp = entry
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let item_type = entry
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let payload = entry.get("payload").unwrap_or(entry);

        match item_type {
            "turn_context" => {
                if let Some(model_name) = payload.get("model").and_then(|v| v.as_str())
                    && !model_name.trim().is_empty()
                {
                    // Keep the latest model for sessions that switched models mid-thread.
                    model = Some(model_name.to_string());
                }
            }
            "response_item" => {
                let response_type = payload
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match response_type {
                    "message" => {
                        let role = payload
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();

                        let mut text_parts: Vec<String> = Vec::new();
                        if let Some(content_arr) = payload.get("content").and_then(|v| v.as_array())
                        {
                            for item in content_arr {
                                let content_type = item
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                if (role == "assistant" || role == "user")
                                    && (content_type == "output_text"
                                        || content_type == "input_text")
                                    && let Some(text) = item.get("text").and_then(|v| v.as_str())
                                {
                                    let trimmed = text.trim();
                                    if !trimmed.is_empty() {
                                        text_parts.push(trimmed.to_string());
                                    }
                                }
                            }
                        }

                        if !text_parts.is_empty() {
                            let joined = text_parts.join("\n");
                            if role == "user" {
                                transcript.add_message(Message::User {
                                    text: joined,
                                    timestamp: timestamp.clone(),
                                });
                            } else if role == "assistant" {
                                transcript.add_message(Message::Assistant {
                                    text: joined,
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                    }
                    "function_call" | "custom_tool_call" | "local_shell_call"
                    | "web_search_call" => {
                        let name = payload
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(response_type)
                            .to_string();

                        let input = if response_type == "function_call" {
                            if let Some(arguments) =
                                payload.get("arguments").and_then(|v| v.as_str())
                            {
                                serde_json::from_str::<serde_json::Value>(arguments).unwrap_or_else(
                                    |_| serde_json::Value::String(arguments.to_string()),
                                )
                            } else {
                                payload.get("arguments").cloned().unwrap_or_else(|| {
                                    serde_json::Value::Object(serde_json::Map::new())
                                })
                            }
                        } else if let Some(input) = payload.get("input").and_then(|v| v.as_str()) {
                            serde_json::Value::String(input.to_string())
                        } else {
                            payload.clone()
                        };

                        transcript.add_message(Message::ToolUse {
                            name,
                            input,
                            timestamp: timestamp.clone(),
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if transcript.messages().is_empty() {
        // Backward-compatible fallback for sessions that only recorded legacy event messages.
        for entry in &parsed_lines {
            let timestamp = entry
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if entry.get("type").and_then(|v| v.as_str()) != Some("event_msg") {
                continue;
            }

            let payload = entry.get("payload").unwrap_or(entry);
            let event_type = payload
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            if event_type == "user_message" {
                if let Some(text) = payload.get("message").and_then(|v| v.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        transcript.add_message(Message::User {
                            text: trimmed.to_string(),
                            timestamp: timestamp.clone(),
                        });
                    }
                }
            } else if event_type == "agent_message"
                && let Some(text) = payload.get("message").and_then(|v| v.as_str())
            {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    transcript.add_message(Message::Assistant {
                        text: trimmed.to_string(),
                        timestamp: timestamp.clone(),
                    });
                }
            }
        }
    }

    Ok((transcript, model))
}

// ---------------------------------------------------------------------------
// Cursor JSONL
// ---------------------------------------------------------------------------

/// Parse a Cursor JSONL transcript file into a transcript.
///
/// Cursor JSONL uses `role` (not `type`) at the top level, has no timestamps
/// or model fields in entries, and wraps user text in `<user_query>` tags.
/// Tool inputs use `path`/`contents` instead of `file_path`/`content`.
pub fn read_cursor_jsonl(path: &Path) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let jsonl_content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;
    let mut transcript = AiTranscript::new();
    let mut plan_states = std::collections::HashMap::new();

    for line in jsonl_content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip malformed lines (file may be partially written)
        let raw_entry: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match raw_entry["role"].as_str() {
            Some("user") => {
                if let Some(content_array) = raw_entry["message"]["content"].as_array() {
                    for item in content_array {
                        if item["type"].as_str() == Some("tool_result") {
                            continue;
                        }
                        if item["type"].as_str() == Some("text")
                            && let Some(text) = item["text"].as_str()
                        {
                            let cleaned = strip_cursor_user_query_tags(text);
                            if !cleaned.is_empty() {
                                transcript.add_message(Message::user(cleaned, None));
                            }
                        }
                    }
                }
            }
            Some("assistant") => {
                if let Some(content_array) = raw_entry["message"]["content"].as_array() {
                    for item in content_array {
                        match item["type"].as_str() {
                            Some("text") => {
                                if let Some(text) = item["text"].as_str()
                                    && !text.trim().is_empty()
                                {
                                    transcript
                                        .add_message(Message::assistant(text.to_string(), None));
                                }
                            }
                            Some("thinking") => {
                                if let Some(thinking) = item["thinking"].as_str()
                                    && !thinking.trim().is_empty()
                                {
                                    transcript.add_message(Message::assistant(
                                        thinking.to_string(),
                                        None,
                                    ));
                                }
                            }
                            Some("tool_use") => {
                                if let Some(name) = item["name"].as_str() {
                                    let input = &item["input"];
                                    // Normalize tool input: Cursor uses `path` where git-ai uses `file_path`
                                    let normalized_input = normalize_cursor_tool_input(name, input);

                                    // Check for plan file writes
                                    if let Some(plan_text) = extract_plan_from_tool_use(
                                        name,
                                        &normalized_input,
                                        &mut plan_states,
                                    ) {
                                        transcript.add_message(Message::Plan {
                                            text: plan_text,
                                            timestamp: None,
                                        });
                                    } else {
                                        // Apply same tool filtering as SQLite path
                                        add_cursor_tool_message(
                                            &mut transcript,
                                            name,
                                            &normalized_input,
                                        );
                                    }
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

    // Model is not in Cursor JSONL -- it comes from hook input
    Ok((transcript, None))
}

/// Strip `<user_query>...</user_query>` wrapper tags from Cursor user messages.
fn strip_cursor_user_query_tags(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<user_query>")
        .and_then(|s| s.strip_suffix("</user_query>"))
    {
        inner.trim().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Normalize Cursor tool input field names to git-ai conventions.
/// Cursor uses `path`/`contents` where git-ai uses `file_path`/`content`.
fn normalize_cursor_tool_input(tool_name: &str, input: &serde_json::Value) -> serde_json::Value {
    let mut normalized = input.clone();
    if let Some(obj) = normalized.as_object_mut() {
        // Rename `path` -> `file_path`
        if let Some(path_val) = obj.remove("path")
            && !obj.contains_key("file_path")
        {
            obj.insert("file_path".to_string(), path_val);
        }
        // For Write tool: rename `contents` -> `content`
        if tool_name == "Write"
            && let Some(contents_val) = obj.remove("contents")
            && !obj.contains_key("content")
        {
            obj.insert("content".to_string(), contents_val);
        }
    }
    normalized
}

/// Add a tool_use message to the transcript. Edit tools store only
/// file_path (content is too large); everything else keeps full args.
fn add_cursor_tool_message(
    transcript: &mut AiTranscript,
    tool_name: &str,
    normalized_input: &serde_json::Value,
) {
    match tool_name {
        // Edit tools: store only file_path (content is too large)
        "Write"
        | "Edit"
        | "StrReplace"
        | "Delete"
        | "MultiEdit"
        | "edit_file"
        | "apply_patch"
        | "edit_file_v2_apply_patch"
        | "search_replace"
        | "edit_file_v2_search_replace" => {
            let file_path = normalized_input
                .get("file_path")
                .and_then(|v| v.as_str())
                .or_else(|| normalized_input.get("target_file").and_then(|v| v.as_str()));
            transcript.add_message(Message::tool_use(
                tool_name.to_string(),
                serde_json::json!({ "file_path": file_path.unwrap_or("") }),
            ));
        }
        // Everything else: store full args
        _ => {
            transcript.add_message(Message::tool_use(
                tool_name.to_string(),
                normalized_input.clone(),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Droid JSONL
// ---------------------------------------------------------------------------

/// Parse a Droid JSONL transcript file into a transcript.
/// Droid JSONL uses the same nested format as Claude Code:
/// `{"type":"message","timestamp":"...","message":{"role":"user|assistant","content":[...]}}`
/// Model is NOT stored in the JSONL -- it comes from the companion .settings.json file.
pub fn read_droid_jsonl(path: &Path) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let jsonl_content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;
    let mut transcript = AiTranscript::new();
    let mut plan_states = std::collections::HashMap::new();

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
                        // Skip tool_result items -- those are system-generated responses
                        if item["type"].as_str() == Some("tool_result") {
                            continue;
                        }
                        if item["type"].as_str() == Some("text")
                            && let Some(text) = item["text"].as_str()
                            && !text.trim().is_empty()
                        {
                            transcript.add_message(Message::User {
                                text: text.to_string(),
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                } else if let Some(content) = message["content"].as_str()
                    && !content.trim().is_empty()
                {
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
                                    && !text.trim().is_empty()
                                {
                                    transcript.add_message(Message::Assistant {
                                        text: text.to_string(),
                                        timestamp: timestamp.clone(),
                                    });
                                }
                            }
                            Some("thinking") => {
                                if let Some(thinking) = item["thinking"].as_str()
                                    && !thinking.trim().is_empty()
                                {
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
                                    // Check if this is a Write/Edit to a plan file
                                    if let Some(plan_text) = extract_plan_from_tool_use(
                                        name,
                                        &item["input"],
                                        &mut plan_states,
                                    ) {
                                        transcript.add_message(Message::Plan {
                                            text: plan_text,
                                            timestamp: timestamp.clone(),
                                        });
                                    } else {
                                        transcript.add_message(Message::ToolUse {
                                            name: name.to_string(),
                                            input: item["input"].clone(),
                                            timestamp: timestamp.clone(),
                                        });
                                    }
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

    // Model is not in the JSONL -- return None
    Ok((transcript, None))
}

// ---------------------------------------------------------------------------
// GitHub Copilot Session JSON
// ---------------------------------------------------------------------------

/// Translate a GitHub Copilot chat session JSON file into an AiTranscript,
/// optional model, and edited filepaths.
/// Returns an empty transcript if running in Codespaces or Remote Containers.
#[allow(clippy::type_complexity)]
pub fn read_copilot_session_json(
    path: &Path,
) -> Result<(AiTranscript, Option<String>, Option<Vec<String>>), GitAiError> {
    // Check if running in Codespaces or Remote Containers - if so, return empty transcript
    let is_codespaces = std::env::var("CODESPACES").ok().as_deref() == Some("true");
    let is_remote_containers = std::env::var("REMOTE_CONTAINERS").ok().as_deref() == Some("true");

    if is_codespaces || is_remote_containers {
        return Ok((AiTranscript::new(), None, Some(Vec::new())));
    }

    // Read the session JSON file.
    // Supports both plain .json (pretty-printed or single-line) and .jsonl files
    // where the session is wrapped in a JSONL envelope on the first line:
    //   {"kind":0,"v":{...session data...}}
    let session_json_str = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;

    // Try parsing the first line as JSON first (handles JSONL and single-line JSON).
    // Fall back to parsing the entire content (handles pretty-printed JSON).
    let first_line = session_json_str.lines().next().unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(first_line)
        .or_else(|_| serde_json::from_str(&session_json_str))
        .map_err(GitAiError::JsonError)?;

    // New VS Code Copilot transcript format (1.109.3+):
    // JSONL event stream with lines like {"type":"session.start","data":{...}}
    if looks_like_copilot_event_stream_root(&parsed) {
        return read_copilot_event_stream_jsonl(&session_json_str);
    }

    // Auto-detect JSONL wrapper: if the parsed value has "kind" and "v" fields,
    // unwrap to use the inner "v" object as the session data
    let is_jsonl = parsed.get("kind").is_some() && parsed.get("v").is_some();
    let mut session_json = if is_jsonl {
        parsed.get("v").unwrap().clone()
    } else {
        parsed
    };

    // Apply incremental patches from subsequent JSONL lines (kind:1 = scalar, kind:2 = array/object)
    if is_jsonl {
        for line in session_json_str.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let patch: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let kind = match patch.get("kind").and_then(|v| v.as_u64()) {
                Some(k) => k,
                None => continue,
            };
            if (kind == 1 || kind == 2)
                && let (Some(key_path), Some(value)) =
                    (patch.get("k").and_then(|v| v.as_array()), patch.get("v"))
            {
                // Walk the key path on session_json, setting the value at the leaf
                let keys: Vec<String> = key_path
                    .iter()
                    .filter_map(|k| {
                        k.as_str()
                            .map(|s| s.to_string())
                            .or_else(|| k.as_u64().map(|n| n.to_string()))
                            .or_else(|| k.as_i64().map(|n| n.to_string()))
                    })
                    .collect();
                if !keys.is_empty() {
                    // Use pointer-based indexing to find the parent, then insert at leaf
                    let json_pointer = if keys.len() == 1 {
                        String::new()
                    } else {
                        format!("/{}", keys[..keys.len() - 1].join("/"))
                    };
                    let leaf_key = &keys[keys.len() - 1];
                    let parent = if json_pointer.is_empty() {
                        Some(&mut session_json)
                    } else {
                        session_json.pointer_mut(&json_pointer)
                    };
                    if let Some(obj) = parent.and_then(|p| p.as_object_mut()) {
                        obj.insert(leaf_key.clone(), value.clone());
                    }
                }
            }
        }
    }

    // Extract the requests array which represents the conversation from start to finish
    let requests = session_json
        .get("requests")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            GitAiError::PresetError("requests array not found in Copilot chat session".to_string())
        })?;

    // Extract session-level model from inputState as fallback
    let session_level_model: Option<String> = session_json
        .get("inputState")
        .and_then(|is| is.get("selectedModel"))
        .and_then(|sm| sm.get("identifier"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

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
                                    && !edited_filepaths.contains(&p)
                                {
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
            && let Some(model_id) = request.get("modelId").and_then(|v| v.as_str())
        {
            detected_model = Some(model_id.to_string());
        }
    }

    // Fall back to session-level model if no per-request modelId was found
    if detected_model.is_none() {
        detected_model = session_level_model;
    }

    Ok((transcript, detected_model, Some(edited_filepaths)))
}

// ---------------------------------------------------------------------------
// GitHub Copilot Event Stream JSONL
// ---------------------------------------------------------------------------

fn looks_like_copilot_event_stream_root(parsed: &serde_json::Value) -> bool {
    parsed
        .get("type")
        .and_then(|v| v.as_str())
        .map(|event_type| {
            parsed.get("data").map(|v| v.is_object()).unwrap_or(false)
                && parsed.get("kind").is_none()
                && (event_type.starts_with("session.")
                    || event_type.starts_with("assistant.")
                    || event_type.starts_with("user.")
                    || event_type.starts_with("tool."))
        })
        .unwrap_or(false)
}

#[allow(clippy::type_complexity)]
pub fn read_copilot_event_stream_jsonl(
    session_jsonl: &str,
) -> Result<(AiTranscript, Option<String>, Option<Vec<String>>), GitAiError> {
    let mut transcript = AiTranscript::new();
    let mut edited_filepaths: Vec<String> = Vec::new();
    let mut detected_model: Option<String> = None;

    for line in session_jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let data = event.get("data");
        let timestamp = event
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if detected_model.is_none()
            && let Some(d) = data
        {
            detected_model = extract_copilot_model_hint(d);
        }

        match event_type {
            "user.message" => {
                if let Some(text) = data
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    transcript.add_message(Message::User {
                        text: text.to_string(),
                        timestamp: timestamp.clone(),
                    });
                }
            }
            "assistant.message" => {
                // Prefer visible assistant content; if empty, use reasoningText as a fallback.
                let assistant_text = data
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        data.and_then(|d| d.get("reasoningText"))
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    });

                if let Some(text) = assistant_text {
                    transcript.add_message(Message::Assistant {
                        text,
                        timestamp: timestamp.clone(),
                    });
                }

                if let Some(tool_requests) = data
                    .and_then(|d| d.get("toolRequests"))
                    .and_then(|v| v.as_array())
                {
                    for request in tool_requests {
                        let name = request
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();

                        let input = request
                            .get("arguments")
                            .map(normalize_copilot_tool_arguments)
                            .unwrap_or(serde_json::Value::Null);

                        collect_copilot_filepaths(&input, &mut edited_filepaths);
                        transcript.add_message(Message::tool_use(name, input));
                    }
                }
            }
            "tool.execution_start" => {
                let name = data
                    .and_then(|d| d.get("toolName"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();

                let input = data
                    .and_then(|d| d.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                collect_copilot_filepaths(&input, &mut edited_filepaths);
                transcript.add_message(Message::tool_use(name, input));
            }
            _ => {}
        }
    }

    Ok((transcript, detected_model, Some(edited_filepaths)))
}

fn normalize_copilot_tool_arguments(value: &serde_json::Value) -> serde_json::Value {
    if let Some(as_str) = value.as_str() {
        serde_json::from_str::<serde_json::Value>(as_str)
            .unwrap_or_else(|_| serde_json::Value::String(as_str.to_string()))
    } else {
        value.clone()
    }
}

fn collect_copilot_filepaths(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                let key_lower = key.to_ascii_lowercase();
                if (key_lower == "filepath"
                    || key_lower == "file_path"
                    || key_lower == "fspath"
                    || key_lower == "path")
                    && let Some(path) = val.as_str()
                {
                    let normalized = path.replace('\\', "/");
                    if !out.contains(&normalized) {
                        out.push(normalized);
                    }
                }
                collect_copilot_filepaths(val, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_copilot_filepaths(item, out);
            }
        }
        serde_json::Value::String(s) => {
            collect_apply_patch_paths_from_text(s, out);
        }
        _ => {}
    }
}

fn extract_copilot_model_hint(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(model_id) = map.get("modelId").and_then(|v| v.as_str())
                && model_id.starts_with("copilot/")
            {
                return Some(model_id.to_string());
            }
            if let Some(model) = map.get("model").and_then(|v| v.as_str())
                && model.starts_with("copilot/")
            {
                return Some(model.to_string());
            }
            if let Some(identifier) = map
                .get("selectedModel")
                .and_then(|v| v.get("identifier"))
                .and_then(|v| v.as_str())
                && identifier.starts_with("copilot/")
            {
                return Some(identifier.to_string());
            }
            for val in map.values() {
                if let Some(found) = extract_copilot_model_hint(val) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(extract_copilot_model_hint),
        serde_json::Value::String(s) => {
            if s.starts_with("copilot/") {
                Some(s.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn collect_apply_patch_paths_from_text(raw: &str, out: &mut Vec<String>) {
    for line in raw.lines() {
        let trimmed = line.trim();
        let maybe_path = trimmed
            .strip_prefix("*** Update File: ")
            .or_else(|| trimmed.strip_prefix("*** Add File: "))
            .or_else(|| trimmed.strip_prefix("*** Delete File: "))
            .or_else(|| trimmed.strip_prefix("*** Move to: "));

        if let Some(path) = maybe_path {
            let path = path.trim();
            if !path.is_empty() && !out.iter().any(|existing| existing == path) {
                out.push(path.to_string());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Amp Thread JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AmpThread {
    id: String,
    #[serde(default)]
    messages: Vec<AmpThreadMessage>,
}

#[derive(Debug, Deserialize)]
struct AmpThreadMessage {
    role: String,
    #[serde(default)]
    content: Vec<AmpThreadContent>,
    #[serde(default)]
    meta: Option<AmpMessageMeta>,
    #[serde(default)]
    usage: Option<AmpMessageUsage>,
}

#[derive(Debug, Deserialize)]
struct AmpMessageMeta {
    #[serde(rename = "sentAt")]
    #[allow(dead_code)]
    sent_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AmpMessageUsage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AmpThreadContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        #[allow(dead_code)]
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "toolUseID")]
        #[allow(dead_code)]
        tool_use_id: String,
    },
    #[serde(other)]
    Unknown,
}

/// Parse an Amp thread JSON file into a transcript and extract model info.
/// Returns (transcript, model, thread_id).
pub fn read_amp_thread_json(
    path: &Path,
) -> Result<(AiTranscript, Option<String>, String), GitAiError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| GitAiError::Generic(format!("Failed to read thread file: {}", e)))?;

    let thread: AmpThread = serde_json::from_str(&content)
        .map_err(|e| GitAiError::Generic(format!("Failed to parse Amp thread JSON: {}", e)))?;

    let (transcript, model) = transcript_and_model_from_amp_thread(&thread);

    Ok((transcript, model, thread.id))
}

fn transcript_and_model_from_amp_thread(thread: &AmpThread) -> (AiTranscript, Option<String>) {
    let mut transcript = AiTranscript::new();
    let mut model = None;

    for message in &thread.messages {
        if model.is_none()
            && message.role == "assistant"
            && let Some(msg_model) = message
                .usage
                .as_ref()
                .and_then(|usage| usage.model.as_ref())
                .filter(|value| !value.trim().is_empty())
        {
            model = Some(msg_model.to_string());
        }

        let timestamp = if message.role == "user" {
            message
                .meta
                .as_ref()
                .and_then(|meta| meta.sent_at)
                .and_then(DateTime::from_timestamp_millis)
                .map(|dt| dt.to_rfc3339())
        } else {
            message
                .usage
                .as_ref()
                .and_then(|usage| usage.timestamp.clone())
        };

        match message.role.as_str() {
            "user" => {
                for content in &message.content {
                    if let AmpThreadContent::Text { text } = content
                        && !text.trim().is_empty()
                    {
                        transcript.add_message(Message::User {
                            text: text.to_string(),
                            timestamp: timestamp.clone(),
                        });
                    }
                }
            }
            "assistant" => {
                for content in &message.content {
                    match content {
                        AmpThreadContent::Text { text } => {
                            if !text.trim().is_empty() {
                                transcript.add_message(Message::Assistant {
                                    text: text.to_string(),
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                        AmpThreadContent::Thinking { thinking } => {
                            if !thinking.trim().is_empty() {
                                transcript.add_message(Message::Assistant {
                                    text: thinking.to_string(),
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                        AmpThreadContent::ToolUse { name, input, .. } => {
                            if !name.trim().is_empty() {
                                transcript.add_message(Message::ToolUse {
                                    name: name.to_string(),
                                    input: input.clone(),
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                        AmpThreadContent::ToolResult { .. } | AmpThreadContent::Unknown => {}
                    }
                }
            }
            _ => {}
        }
    }

    (transcript, model)
}

// ---------------------------------------------------------------------------
// OpenCode SQLite
// ---------------------------------------------------------------------------

/// Message metadata from legacy file storage message/{session_id}/{msg_id}.json
#[derive(Debug, Deserialize)]
struct OpenCodeMessage {
    id: String,
    #[serde(rename = "sessionID", default)]
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

/// SQLite message payload from message.data
#[derive(Debug, Deserialize)]
struct OpenCodeDbMessageData {
    role: String,
    #[serde(default)]
    time: Option<OpenCodeTime>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
}

#[derive(Debug)]
struct OpenCodeTranscriptSourceMessage {
    id: String,
    role: String,
    created: i64,
    model_id: Option<String>,
    provider_id: Option<String>,
}

/// Tool state object containing status and nested data
#[derive(Debug, Deserialize)]
struct OpenCodeToolState {
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

/// Part content from either legacy part/{msg_id}/{prt_id}.json or sqlite part.data
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
enum OpenCodePart {
    Text {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        text: String,
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        synthetic: Option<bool>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    Tool {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        tool: String,
        #[serde(rename = "callID")]
        #[allow(dead_code)]
        call_id: String,
        state: Option<OpenCodeToolState>,
        input: Option<serde_json::Value>,
        #[allow(dead_code)]
        output: Option<serde_json::Value>,
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepStart {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        #[allow(dead_code)]
        time: Option<OpenCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepFinish {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
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

/// Read a transcript from an OpenCode SQLite database.
pub fn read_opencode_sqlite(
    db_path: &Path,
    session_id: &str,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let conn = opencode_open_sqlite_readonly(db_path)?;
    let messages = opencode_read_session_messages_from_sqlite(&conn, session_id)?;

    if messages.is_empty() {
        return Ok((AiTranscript::new(), None));
    }

    // Batch-load all parts for the session in a single query instead of
    // one query per message (N+1). Without indexes on the OpenCode DB,
    // each query requires a full table scan -- doing this once instead of
    // N times prevents extreme memory/CPU usage on large databases.
    let mut parts_by_message = opencode_read_all_session_parts_from_sqlite(&conn, session_id)?;

    opencode_build_transcript_from_messages(messages, |message_id| {
        Ok(parts_by_message.remove(message_id).unwrap_or_default())
    })
}

/// Read a transcript from OpenCode legacy JSON file storage.
pub fn read_opencode_legacy_json(
    storage_path: &Path,
    session_id: &str,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    if !storage_path.exists() {
        return Err(GitAiError::PresetError(format!(
            "OpenCode legacy storage path does not exist: {:?}",
            storage_path
        )));
    }

    let messages = opencode_read_session_messages(storage_path, session_id)?;
    if messages.is_empty() {
        return Ok((AiTranscript::new(), None));
    }

    opencode_build_transcript_from_messages(messages, |message_id| {
        opencode_read_message_parts(storage_path, message_id)
    })
}

fn opencode_open_sqlite_readonly(path: &Path) -> Result<Connection, GitAiError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| GitAiError::Generic(format!("Failed to open {:?}: {}", path, e)))?;

    // Limit SQLite page cache to ~2MB to prevent unbounded memory growth
    // when scanning large databases without indexes.
    // Default can grow much larger during repeated full table scans.
    let _ = conn.execute_batch("PRAGMA cache_size = -2000;");

    Ok(conn)
}

fn opencode_build_transcript_from_messages<F>(
    mut messages: Vec<OpenCodeTranscriptSourceMessage>,
    mut read_parts: F,
) -> Result<(AiTranscript, Option<String>), GitAiError>
where
    F: FnMut(&str) -> Result<Vec<OpenCodePart>, GitAiError>,
{
    messages.sort_by_key(|m| m.created);

    let mut transcript = AiTranscript::new();
    let mut model: Option<String> = None;

    for message in &messages {
        // Extract model from first assistant message
        if model.is_none() && message.role == "assistant" {
            if let (Some(provider_id), Some(model_id)) = (&message.provider_id, &message.model_id) {
                model = Some(format!("{}/{}", provider_id, model_id));
            } else if let Some(model_id) = &message.model_id {
                model = Some(model_id.clone());
            }
        }

        let parts = read_parts(&message.id)?;

        // Convert Unix ms to RFC3339 timestamp
        let timestamp = DateTime::from_timestamp_millis(message.created).map(|dt| dt.to_rfc3339());

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
                    tool, input, state, ..
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

fn opencode_part_created_for_sort(part: &OpenCodePart, fallback: i64) -> i64 {
    match part {
        OpenCodePart::Text { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
        OpenCodePart::Tool { time, state, .. } => time
            .as_ref()
            .map(|t| t.start)
            .or_else(|| {
                state
                    .as_ref()
                    .and_then(|s| s.time.as_ref())
                    .map(|t| t.start)
            })
            .unwrap_or(fallback),
        OpenCodePart::StepStart { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
        OpenCodePart::StepFinish { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
        OpenCodePart::Unknown => fallback,
    }
}

/// Read all legacy message files for a session
fn opencode_read_session_messages(
    storage_path: &Path,
    session_id: &str,
) -> Result<Vec<OpenCodeTranscriptSourceMessage>, GitAiError> {
    let message_dir = storage_path.join("message").join(session_id);
    if !message_dir.exists() {
        return Ok(Vec::new());
    }

    let mut messages = Vec::new();

    let entries = std::fs::read_dir(&message_dir).map_err(GitAiError::IoError)?;

    for entry in entries {
        let entry = entry.map_err(GitAiError::IoError)?;
        let path = entry.path();

        if path.extension().is_some_and(|ext| ext == "json") {
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<OpenCodeMessage>(&content) {
                    Ok(message) => messages.push(OpenCodeTranscriptSourceMessage {
                        id: message.id,
                        role: message.role,
                        created: message.time.created,
                        model_id: message.model_id,
                        provider_id: message.provider_id,
                    }),
                    Err(e) => {
                        eprintln!(
                            "[Warning] Failed to parse OpenCode message file {:?}: {}",
                            path, e
                        );
                    }
                },
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

fn opencode_read_message_parts(
    storage_path: &Path,
    message_id: &str,
) -> Result<Vec<OpenCodePart>, GitAiError> {
    let part_dir = storage_path.join("part").join(message_id);
    if !part_dir.exists() {
        return Ok(Vec::new());
    }

    let mut parts: Vec<(i64, OpenCodePart)> = Vec::new();
    let entries = std::fs::read_dir(&part_dir).map_err(GitAiError::IoError)?;

    for entry in entries {
        let entry = entry.map_err(GitAiError::IoError)?;
        let path = entry.path();

        if path.extension().is_some_and(|ext| ext == "json") {
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<OpenCodePart>(&content) {
                    Ok(part) => {
                        let created = opencode_part_created_for_sort(&part, 0);
                        parts.push((created, part));
                    }
                    Err(e) => {
                        eprintln!(
                            "[Warning] Failed to parse OpenCode part file {:?}: {}",
                            path, e
                        );
                    }
                },
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

fn opencode_read_session_messages_from_sqlite(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<OpenCodeTranscriptSourceMessage>, GitAiError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, time_created, data FROM message WHERE session_id = ? ORDER BY time_created ASC, id ASC",
        )
        .map_err(|e| GitAiError::Generic(format!("SQLite query prepare failed: {}", e)))?;

    let mut rows = stmt
        .query([session_id])
        .map_err(|e| GitAiError::Generic(format!("SQLite query failed: {}", e)))?;

    let mut messages = Vec::new();

    while let Some(row) = rows
        .next()
        .map_err(|e| GitAiError::Generic(format!("SQLite row read failed: {}", e)))?
    {
        let id: String = row
            .get(0)
            .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;
        let created_column: i64 = row
            .get(1)
            .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;
        let data_text: String = row
            .get(2)
            .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;

        match serde_json::from_str::<OpenCodeDbMessageData>(&data_text) {
            Ok(data) => {
                let OpenCodeDbMessageData {
                    role,
                    time,
                    model_id,
                    provider_id,
                } = data;
                messages.push(OpenCodeTranscriptSourceMessage {
                    id,
                    role,
                    created: time.map(|t| t.created).unwrap_or(created_column),
                    model_id,
                    provider_id,
                });
            }
            Err(e) => {
                eprintln!(
                    "[Warning] Failed to parse OpenCode sqlite message row {}: {}",
                    id, e
                );
            }
        }
    }

    Ok(messages)
}

/// Read all parts for a session in a single query, grouped by message_id.
/// This avoids N+1 full table scans on databases without indexes.
fn opencode_read_all_session_parts_from_sqlite(
    conn: &Connection,
    session_id: &str,
) -> Result<HashMap<String, Vec<OpenCodePart>>, GitAiError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, message_id, time_created, data FROM part WHERE session_id = ? ORDER BY message_id ASC, id ASC",
        )
        .map_err(|e| GitAiError::Generic(format!("SQLite query prepare failed: {}", e)))?;

    let mut rows = stmt
        .query([session_id])
        .map_err(|e| GitAiError::Generic(format!("SQLite query failed: {}", e)))?;

    let mut parts_by_message: HashMap<String, Vec<(i64, OpenCodePart)>> = HashMap::new();

    while let Some(row) = rows
        .next()
        .map_err(|e| GitAiError::Generic(format!("SQLite row read failed: {}", e)))?
    {
        let part_id: String = row
            .get(0)
            .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;
        let message_id: String = row
            .get(1)
            .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;
        let created_column: i64 = row
            .get(2)
            .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;
        let data_text: String = row
            .get(3)
            .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;

        match serde_json::from_str::<OpenCodePart>(&data_text) {
            Ok(part) => {
                let created = opencode_part_created_for_sort(&part, created_column);
                parts_by_message
                    .entry(message_id)
                    .or_default()
                    .push((created, part));
            }
            Err(e) => {
                eprintln!(
                    "[Warning] Failed to parse OpenCode sqlite part row {}: {}",
                    part_id, e
                );
            }
        }
    }

    // Sort each message's parts by creation time
    let mut result: HashMap<String, Vec<OpenCodePart>> = HashMap::new();
    for (message_id, mut parts) in parts_by_message {
        parts.sort_by_key(|(created, _)| *created);
        result.insert(
            message_id,
            parts.into_iter().map(|(_, part)| part).collect(),
        );
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Pi JSONL
// ---------------------------------------------------------------------------

/// Parse a Pi session JSONL file into a transcript and extract model info.
pub fn read_pi_jsonl(path: &Path) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to read Pi session file {}: {e}",
            path.display()
        ))
    })?;

    let mut transcript = AiTranscript::new();
    let mut latest_model: Option<String> = None;
    let mut saw_session_header = false;

    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            GitAiError::Generic(format!(
                "Failed to parse Pi session JSONL line {} in {}: {e}",
                index + 1,
                path.display()
            ))
        })?;

        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("session") => {
                saw_session_header = true;
            }
            Some("message") => {
                pi_push_message_entry(&mut transcript, &mut latest_model, &entry)?;
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
            path.display()
        )));
    }

    Ok((transcript, latest_model))
}

fn pi_push_message_entry(
    transcript: &mut AiTranscript,
    latest_model: &mut Option<String>,
    entry: &serde_json::Value,
) -> Result<(), GitAiError> {
    let message = entry
        .get("message")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            GitAiError::Generic("Pi message entry missing message object".to_string())
        })?;
    let role = message
        .get("role")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GitAiError::Generic("Pi message entry missing role".to_string()))?;
    let timestamp = pi_message_timestamp(entry, message.get("timestamp"));

    match role {
        "user" => pi_push_user_message(transcript, message.get("content"), timestamp),
        "assistant" => {
            if let Some(model) = pi_assistant_model(message)
                && !model.trim().is_empty()
            {
                *latest_model = Some(model);
            }
            pi_push_assistant_message(transcript, message.get("content"), timestamp);
        }
        "toolResult" => {
            pi_push_tool_result_message(transcript, message.get("content"), timestamp);
        }
        "custom" | "branchSummary" | "compactionSummary" => {}
        _ => {}
    }

    Ok(())
}

fn pi_push_user_message(
    transcript: &mut AiTranscript,
    content: Option<&serde_json::Value>,
    timestamp: Option<String>,
) {
    match content {
        Some(serde_json::Value::String(text)) => {
            if !text.trim().is_empty() {
                transcript.add_message(Message::User {
                    text: text.to_string(),
                    timestamp,
                });
            }
        }
        Some(serde_json::Value::Array(blocks)) => {
            for block in blocks {
                if block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    && let Some(text) = block.get("text").and_then(serde_json::Value::as_str)
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

fn pi_push_assistant_message(
    transcript: &mut AiTranscript,
    content: Option<&serde_json::Value>,
    timestamp: Option<String>,
) {
    let Some(serde_json::Value::Array(blocks)) = content else {
        return;
    };

    for block in blocks {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str)
                    && !text.trim().is_empty()
                {
                    transcript.add_message(Message::Assistant {
                        text: text.to_string(),
                        timestamp: timestamp.clone(),
                    });
                }
            }
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(serde_json::Value::as_str)
                    && !text.trim().is_empty()
                {
                    transcript.add_message(Message::Thinking {
                        text: text.to_string(),
                        timestamp: timestamp.clone(),
                    });
                }
            }
            Some("toolCall") => {
                if let Some(name) = block.get("name").and_then(serde_json::Value::as_str) {
                    let input = block
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
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

fn pi_push_tool_result_message(
    transcript: &mut AiTranscript,
    content: Option<&serde_json::Value>,
    timestamp: Option<String>,
) {
    let Some(serde_json::Value::Array(blocks)) = content else {
        return;
    };

    for block in blocks {
        if block.get("type").and_then(serde_json::Value::as_str) == Some("text")
            && let Some(text) = block.get("text").and_then(serde_json::Value::as_str)
            && !text.trim().is_empty()
        {
            transcript.add_message(Message::Assistant {
                text: text.to_string(),
                timestamp: timestamp.clone(),
            });
        }
    }
}

fn pi_assistant_model(message: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let model = message.get("model").and_then(serde_json::Value::as_str)?;
    if model.trim().is_empty() {
        return None;
    }
    Some(model.to_string())
}

fn pi_message_timestamp(
    entry: &serde_json::Value,
    message_timestamp: Option<&serde_json::Value>,
) -> Option<String> {
    if let Some(timestamp_ms) = message_timestamp.and_then(serde_json::Value::as_i64)
        && let Some(timestamp) = DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
    {
        return Some(timestamp.to_rfc3339());
    }

    entry
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .map(|value| value.to_string())
}

// ---------------------------------------------------------------------------
// Continue CLI JSON
// ---------------------------------------------------------------------------

/// Parse a Continue CLI JSON file into a transcript.
pub fn read_continue_json(path: &Path) -> Result<AiTranscript, GitAiError> {
    let json_content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;
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
        let message = match history_item.get("message") {
            Some(m) => m,
            None => continue,
        };

        let role = match message.get("role").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => continue,
        };

        let timestamp = message
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match role {
            "user" => {
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
                if let Some(content) = message.get("content") {
                    match content {
                        serde_json::Value::String(text) => {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                transcript.add_message(Message::Assistant {
                                    text: trimmed.to_string(),
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                        serde_json::Value::Array(parts) => {
                            for part in parts {
                                let part_type = part
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                if part_type == "text"
                                    && let Some(text) = part.get("text").and_then(|v| v.as_str())
                                    && !text.trim().is_empty()
                                {
                                    transcript.add_message(Message::Assistant {
                                        text: text.trim().to_string(),
                                        timestamp: timestamp.clone(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // Extract tool use from contextItems
                if let Some(context_items) =
                    history_item.get("contextItems").and_then(|v| v.as_array())
                {
                    for item in context_items {
                        if let Some(name) = item.get("name").and_then(|v| v.as_str())
                            && !name.trim().is_empty()
                        {
                            let input = item.get("content").cloned().unwrap_or_else(|| {
                                serde_json::Value::Object(serde_json::Map::new())
                            });
                            transcript.add_message(Message::ToolUse {
                                name: name.to_string(),
                                input,
                                timestamp: timestamp.clone(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(transcript)
}

// ---------------------------------------------------------------------------
// Droid settings helper
// ---------------------------------------------------------------------------

/// Read model name from a Droid settings.json file.
pub fn read_droid_model_from_settings(path: &Path) -> Result<Option<String>, GitAiError> {
    let content = std::fs::read_to_string(path).map_err(GitAiError::IoError)?;
    let settings: serde_json::Value =
        serde_json::from_str(&content).map_err(GitAiError::JsonError)?;
    Ok(settings["model"].as_str().map(|s| s.to_string()))
}

// ---------------------------------------------------------------------------
// Amp thread discovery helpers
// ---------------------------------------------------------------------------

/// Get the default Amp threads directory based on platform.
pub fn amp_threads_path() -> Result<std::path::PathBuf, GitAiError> {
    if let Ok(test_path) = std::env::var("GIT_AI_AMP_THREADS_PATH") {
        return Ok(std::path::PathBuf::from(test_path));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
            return Ok(std::path::PathBuf::from(xdg_data)
                .join("amp")
                .join("threads"));
        }

        let home = dirs::home_dir()
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home
            .join(".local")
            .join("share")
            .join("amp")
            .join("threads"))
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            return Ok(std::path::PathBuf::from(local_app_data)
                .join("amp")
                .join("threads"));
        }
        if let Ok(app_data) = std::env::var("APPDATA") {
            return Ok(std::path::PathBuf::from(app_data)
                .join("amp")
                .join("threads"));
        }

        let home = dirs::home_dir()
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home
            .join("AppData")
            .join("Local")
            .join("amp")
            .join("threads"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(GitAiError::Generic(
            "Amp threads path not supported on this platform".to_string(),
        ))
    }
}

/// Read transcript from an Amp thread ID (resolves to default threads dir).
pub fn read_amp_thread_by_id(
    thread_id: &str,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let thread_path = amp_threads_path()?.join(format!("{}.json", thread_id));
    let (transcript, model, _) = read_amp_thread_json(&thread_path)?;
    Ok((transcript, model))
}

/// Read transcript from an Amp thread ID in a specific directory.
pub fn read_amp_thread_by_id_in_dir(
    threads_dir: &Path,
    thread_id: &str,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let thread_path = threads_dir.join(format!("{}.json", thread_id));
    let (transcript, model, _) = read_amp_thread_json(&thread_path)?;
    Ok((transcript, model))
}

/// Find and read an Amp thread by tool_use_id in a directory.
pub fn read_amp_thread_by_tool_use_id_in_dir(
    threads_dir: &Path,
    tool_use_id: &str,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let thread_path =
        find_amp_thread_file_by_tool_use_id(threads_dir, tool_use_id)?.ok_or_else(|| {
            GitAiError::Generic(format!(
                "No Amp thread file found for tool_use_id {} in {}",
                tool_use_id,
                threads_dir.display()
            ))
        })?;
    let (transcript, model, _) = read_amp_thread_json(&thread_path)?;
    Ok((transcript, model))
}

fn find_amp_thread_file_by_tool_use_id(
    threads_path: &Path,
    tool_use_id: &str,
) -> Result<Option<std::path::PathBuf>, GitAiError> {
    if !threads_path.exists() {
        return Ok(None);
    }

    let mut newest_match: Option<(std::path::PathBuf, std::time::SystemTime)> = None;

    for entry in std::fs::read_dir(threads_path)? {
        let entry = entry?;
        let path = entry.path();
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            continue;
        }

        if !amp_thread_file_contains_tool_use_id(&path, tool_use_id)? {
            continue;
        }

        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH);

        match &newest_match {
            Some((_, newest_modified)) if modified <= *newest_modified => {}
            _ => newest_match = Some((path, modified)),
        }
    }

    Ok(newest_match.map(|(path, _)| path))
}

fn amp_thread_file_contains_tool_use_id(
    thread_path: &Path,
    tool_use_id: &str,
) -> Result<bool, GitAiError> {
    let content = std::fs::read_to_string(thread_path)
        .map_err(|e| GitAiError::Generic(format!("Failed to read thread file: {}", e)))?;

    if !content.contains(tool_use_id) {
        return Ok(false);
    }

    let parsed: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| GitAiError::Generic(format!("Failed to parse Amp thread JSON: {}", e)))?;

    let messages = parsed
        .get("messages")
        .and_then(|messages| messages.as_array())
        .cloned()
        .unwrap_or_default();

    for message in messages {
        let content_items = message
            .get("content")
            .and_then(|content| content.as_array())
            .cloned()
            .unwrap_or_default();

        for item in content_items {
            let item_type = item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if item_type == "tool_use"
                && item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|value| value == tool_use_id)
            {
                return Ok(true);
            }
            if item_type == "tool_result"
                && item
                    .get("toolUseID")
                    .and_then(|v| v.as_str())
                    .is_some_and(|value| value == tool_use_id)
            {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

// ---------------------------------------------------------------------------
// OpenCode discovery helpers
// ---------------------------------------------------------------------------

/// Get the OpenCode data directory based on platform.
pub fn opencode_data_path() -> Result<std::path::PathBuf, GitAiError> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir()
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home.join(".local").join("share").join("opencode"))
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
            Ok(std::path::PathBuf::from(xdg_data).join("opencode"))
        } else {
            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            Ok(home.join(".local").join("share").join("opencode"))
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(app_data) = std::env::var("APPDATA") {
            Ok(std::path::PathBuf::from(app_data).join("opencode"))
        } else if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            Ok(std::path::PathBuf::from(local_app_data).join("opencode"))
        } else {
            Err(GitAiError::Generic(
                "Neither APPDATA nor LOCALAPPDATA is set".to_string(),
            ))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(GitAiError::PresetError(
            "OpenCode storage path not supported on this platform".to_string(),
        ))
    }
}

/// Fetch transcript and model from OpenCode using default data path.
pub fn read_opencode_from_session(
    session_id: &str,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    let opencode_path = opencode_data_path()?;
    read_opencode_from_storage(&opencode_path, session_id)
}

/// Fetch transcript and model from OpenCode path (sqlite first, fallback to legacy storage).
///
/// `opencode_path` may be one of:
/// - OpenCode data dir (contains `opencode.db` and optional `storage/`)
/// - Legacy storage dir (contains `message/` and `part/`)
/// - Direct path to `opencode.db`
pub fn read_opencode_from_storage(
    opencode_path: &Path,
    session_id: &str,
) -> Result<(AiTranscript, Option<String>), GitAiError> {
    if !opencode_path.exists() {
        return Err(GitAiError::PresetError(format!(
            "OpenCode path does not exist: {:?}",
            opencode_path
        )));
    }

    let mut sqlite_empty_result: Option<(AiTranscript, Option<String>)> = None;
    let mut sqlite_error: Option<GitAiError> = None;

    if let Some(db_path) = resolve_opencode_sqlite_db_path(opencode_path) {
        match read_opencode_sqlite(&db_path, session_id) {
            Ok((transcript, model)) => {
                if !transcript.messages().is_empty() || model.is_some() {
                    return Ok((transcript, model));
                }
                sqlite_empty_result = Some((transcript, model));
            }
            Err(e) => {
                eprintln!(
                    "[Warning] Failed to parse OpenCode sqlite db {:?}: {}",
                    db_path, e
                );
                sqlite_error = Some(e);
            }
        }
    }

    if let Some(storage_path) = resolve_opencode_legacy_storage_path(opencode_path) {
        match read_opencode_legacy_json(&storage_path, session_id) {
            Ok((transcript, model)) => {
                if !transcript.messages().is_empty() || model.is_some() {
                    return Ok((transcript, model));
                }
                if let Some(result) = sqlite_empty_result.take() {
                    return Ok(result);
                }
                return Ok((transcript, model));
            }
            Err(e) => {
                if let Some(result) = sqlite_empty_result.take() {
                    return Ok(result);
                }
                if let Some(sqlite_err) = sqlite_error {
                    return Err(sqlite_err);
                }
                return Err(e);
            }
        }
    }

    if let Some(result) = sqlite_empty_result {
        return Ok(result);
    }

    if let Some(sqlite_err) = sqlite_error {
        return Err(sqlite_err);
    }

    Err(GitAiError::PresetError(format!(
        "No OpenCode sqlite database or legacy storage found under {:?}",
        opencode_path
    )))
}

fn resolve_opencode_sqlite_db_path(path: &Path) -> Option<std::path::PathBuf> {
    if path.is_file() {
        return path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| *name == "opencode.db")
            .map(|_| path.to_path_buf());
    }

    if !path.is_dir() {
        return None;
    }

    let direct_db = path.join("opencode.db");
    if direct_db.exists() {
        return Some(direct_db);
    }

    // If caller passed legacy storage path, check sibling opencode.db
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "storage")
    {
        let sibling_db = path.parent()?.join("opencode.db");
        if sibling_db.exists() {
            return Some(sibling_db);
        }
    }

    None
}

fn resolve_opencode_legacy_storage_path(path: &Path) -> Option<std::path::PathBuf> {
    if path.is_file() {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "opencode.db")
        {
            let storage = path.parent()?.join("storage");
            if storage.exists() {
                return Some(storage);
            }
        }
        return None;
    }

    if !path.is_dir() {
        return None;
    }

    // Direct storage directory
    if path.join("message").exists() || path.join("part").exists() {
        return Some(path.to_path_buf());
    }

    // Subdirectory named storage
    let sub = path.join("storage");
    if sub.exists() {
        return Some(sub);
    }

    None
}

// ---------------------------------------------------------------------------
// Codex filesystem helpers
// ---------------------------------------------------------------------------

/// Get the Codex home directory.
pub fn codex_home_dir() -> std::path::PathBuf {
    if let Ok(codex_home) = std::env::var("CODEX_HOME")
        && !codex_home.trim().is_empty()
    {
        return std::path::PathBuf::from(codex_home);
    }

    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("~"))
        .join(".codex")
}

/// Find the latest Codex rollout file for a given session ID.
pub fn find_codex_rollout_path_for_session(
    session_id: &str,
) -> Result<Option<std::path::PathBuf>, GitAiError> {
    find_codex_rollout_path_for_session_in_home(session_id, &codex_home_dir())
}

/// Find the latest Codex rollout file for a given session ID in a specific home dir.
pub fn find_codex_rollout_path_for_session_in_home(
    session_id: &str,
    codex_home: &Path,
) -> Result<Option<std::path::PathBuf>, GitAiError> {
    let mut candidates = Vec::new();
    for subdir in ["sessions", "archived_sessions"] {
        let base = codex_home.join(subdir);
        if !base.exists() {
            continue;
        }

        let pattern = format!(
            "{}/**/rollout-*{}*.jsonl",
            base.to_string_lossy(),
            session_id
        );
        let entries = glob::glob(&pattern)
            .map_err(|e| GitAiError::Generic(format!("Failed to glob Codex rollout files: {e}")))?;

        for entry in entries.flatten() {
            if entry.is_file() {
                candidates.push(entry);
            }
        }
    }

    let newest = candidates.into_iter().max_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });

    Ok(newest)
}

// ---------------------------------------------------------------------------
// Pi convenience wrapper
// ---------------------------------------------------------------------------

/// Read transcript from a Pi session file path (string form).
pub fn read_pi_session(session_path: &str) -> Result<(AiTranscript, Option<String>), GitAiError> {
    read_pi_jsonl(Path::new(session_path))
}
