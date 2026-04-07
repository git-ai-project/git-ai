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
        bash_tool::{self, Agent, BashCheckpointAction, HookEvent, ToolClass},
    },
    error::GitAiError,
    observability::log_error,
};
use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};
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
    tool_input: Option<serde_json::Value>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default, alias = "toolUseId")]
    tool_use_id: Option<String>,
}

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
struct TranscriptSourceMessage {
    id: String,
    role: String,
    created: i64,
    model_id: Option<String>,
    provider_id: Option<String>,
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
        state: Option<ToolState>,
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

impl AgentCheckpointPreset for OpenCodePreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for OpenCode preset".to_string())
        })?;

        let hook_input: OpenCodeHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        // Determine if this is a bash tool invocation (before destructuring)
        let is_bash_tool = hook_input
            .tool_name
            .as_deref()
            .map(|name| bash_tool::classify_tool(Agent::OpenCode, name) == ToolClass::Bash)
            .unwrap_or(false);

        let OpenCodeHookInput {
            hook_event_name,
            session_id,
            cwd,
            tool_input,
            tool_name: _,
            tool_use_id,
        } = hook_input;

        let file_paths = Self::extract_filepaths_from_tool_input(tool_input.as_ref(), &cwd);

        // Determine OpenCode path (test override can point to either root or legacy storage path)
        let opencode_path = if let Ok(test_path) = std::env::var("GIT_AI_OPENCODE_STORAGE_PATH") {
            PathBuf::from(test_path)
        } else {
            Self::opencode_data_path()?
        };

        // Fetch transcript and model from sqlite first, then fallback to legacy storage
        let (transcript, model) =
            match Self::transcript_and_model_from_storage(&opencode_path, &session_id) {
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
        // Store test path if set, for subprocess access in tests
        if let Ok(test_path) = std::env::var("GIT_AI_OPENCODE_STORAGE_PATH") {
            agent_metadata.insert("__test_storage_path".to_string(), test_path);
        }

        let tool_use_id = tool_use_id.as_deref().unwrap_or("bash");

        // Check if this is a PreToolUse event (human checkpoint)
        if hook_event_name == "PreToolUse" {
            let pre_hook_captured_id = prepare_agent_bash_pre_hook(
                is_bash_tool,
                Some(&cwd),
                &agent_id.id,
                tool_use_id,
                &agent_id,
                Some(&agent_metadata),
                BashPreHookStrategy::EmitHumanCheckpoint,
            )?
            .captured_checkpoint_id();
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: Some(cwd),
                edited_filepaths: None,
                will_edit_filepaths: file_paths,
                dirty_files: None,
                captured_checkpoint_id: pre_hook_captured_id,
            });
        }

        // PostToolUse: for bash tools, diff snapshots to detect changed files
        let bash_result = if is_bash_tool {
            let repo_root = Path::new(&cwd);
            Some(bash_tool::handle_bash_tool(
                HookEvent::PostToolUse,
                repo_root,
                &agent_id.id,
                tool_use_id,
            ))
        } else {
            None
        };
        let edited_filepaths = if is_bash_tool {
            match bash_result.as_ref().unwrap().as_ref().map(|r| &r.action) {
                Ok(BashCheckpointAction::Checkpoint(paths)) => Some(paths.clone()),
                Ok(BashCheckpointAction::NoChanges) => None,
                Ok(BashCheckpointAction::Fallback) => {
                    // snapshot unavailable or repo too large; no paths to report
                    None
                }
                Ok(BashCheckpointAction::TakePreSnapshot) => None,
                Err(e) => {
                    crate::utils::debug_log(&format!("Bash tool post-hook error: {}", e));
                    None
                }
            }
        } else {
            file_paths
        };

        let bash_captured_checkpoint_id = bash_result
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|r| r.captured_checkpoint.as_ref())
            .map(|info| info.capture_id.clone());

        // PostToolUse event - AI checkpoint
        Ok(AgentRunResult {
            agent_id,
            agent_metadata: Some(agent_metadata),
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: Some(cwd),
            edited_filepaths,
            will_edit_filepaths: None,
            dirty_files: None,
            captured_checkpoint_id: bash_captured_checkpoint_id,
        })
    }
}

impl OpenCodePreset {
    fn extract_filepaths_from_tool_input(
        tool_input: Option<&serde_json::Value>,
        cwd: &str,
    ) -> Option<Vec<String>> {
        let mut raw_paths = Vec::new();

        if let Some(value) = tool_input {
            Self::collect_tool_paths(value, &mut raw_paths);
        }

        let mut normalized_paths = Vec::new();
        for raw in raw_paths {
            if let Some(path) = Self::normalize_hook_path(&raw, cwd)
                && !normalized_paths.contains(&path)
            {
                normalized_paths.push(path);
            }
        }

        if normalized_paths.is_empty() {
            None
        } else {
            Some(normalized_paths)
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
                let path = path.trim().trim_matches('"').trim_matches('\'');
                if !path.is_empty() && !out.iter().any(|existing| existing == path) {
                    out.push(path.to_string());
                }
            }
        }
    }

    fn collect_tool_paths(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map {
                    let key_lower = key.to_ascii_lowercase();
                    let is_single_path_key = key_lower == "file_path"
                        || key_lower == "filepath"
                        || key_lower == "path"
                        || key_lower == "fspath";

                    let is_multi_path_key = key_lower == "files"
                        || key_lower == "filepaths"
                        || key_lower == "file_paths";

                    if is_single_path_key {
                        if let Some(path) = val.as_str() {
                            out.push(path.to_string());
                        }
                    } else if is_multi_path_key {
                        match val {
                            serde_json::Value::String(path) => out.push(path.to_string()),
                            serde_json::Value::Array(paths) => {
                                for path_value in paths {
                                    if let Some(path) = path_value.as_str() {
                                        out.push(path.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    Self::collect_tool_paths(val, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    Self::collect_tool_paths(item, out);
                }
            }
            serde_json::Value::String(s) => {
                if s.starts_with("file://") {
                    out.push(s.to_string());
                }
                Self::collect_apply_patch_paths_from_text(s, out);
            }
            _ => {}
        }
    }

    fn normalize_hook_path(raw_path: &str, cwd: &str) -> Option<String> {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return None;
        }

        let path_without_scheme = trimmed
            .strip_prefix("file://localhost")
            .or_else(|| trimmed.strip_prefix("file://"))
            .unwrap_or(trimmed);

        let path = Path::new(path_without_scheme);
        let joined = if path.is_absolute()
            || path_without_scheme.starts_with("\\\\")
            || path_without_scheme
                .as_bytes()
                .get(1)
                .map(|b| *b == b':')
                .unwrap_or(false)
        {
            PathBuf::from(path_without_scheme)
        } else {
            Path::new(cwd).join(path_without_scheme)
        };

        Some(joined.to_string_lossy().replace('\\', "/"))
    }

    /// Get the OpenCode data directory based on platform.
    /// Expected layout: {data_dir}/opencode.db and {data_dir}/storage
    pub fn opencode_data_path() -> Result<PathBuf, GitAiError> {
        #[cfg(target_os = "macos")]
        {
            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            Ok(home.join(".local").join("share").join("opencode"))
        }

        #[cfg(target_os = "linux")]
        {
            // Try XDG_DATA_HOME first, then fall back to ~/.local/share
            if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
                Ok(PathBuf::from(xdg_data).join("opencode"))
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
                Ok(PathBuf::from(app_data).join("opencode"))
            } else if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                Ok(PathBuf::from(local_app_data).join("opencode"))
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

    /// Public API for fetching transcript from session_id (uses default OpenCode data path)
    pub fn transcript_and_model_from_session(
        session_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let opencode_path = Self::opencode_data_path()?;
        Self::transcript_and_model_from_storage(&opencode_path, session_id)
    }

    /// Fetch transcript and model from OpenCode path (sqlite first, fallback to legacy storage)
    ///
    /// `opencode_path` may be one of:
    /// - OpenCode data dir (contains `opencode.db` and optional `storage/`)
    /// - Legacy storage dir (contains `message/` and `part/`)
    /// - Direct path to `opencode.db`
    pub fn transcript_and_model_from_storage(
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

        if let Some(db_path) = Self::resolve_sqlite_db_path(opencode_path) {
            match Self::transcript_and_model_from_sqlite(&db_path, session_id) {
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

        if let Some(storage_path) = Self::resolve_legacy_storage_path(opencode_path) {
            match Self::transcript_and_model_from_legacy_storage(&storage_path, session_id) {
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

    fn resolve_sqlite_db_path(path: &Path) -> Option<PathBuf> {
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

    fn resolve_legacy_storage_path(path: &Path) -> Option<PathBuf> {
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

        if path.join("message").exists() || path.join("part").exists() {
            return Some(path.to_path_buf());
        }

        let nested_storage = path.join("storage");
        if nested_storage.exists() {
            return Some(nested_storage);
        }

        None
    }

    fn open_sqlite_readonly(path: &Path) -> Result<Connection, GitAiError> {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| GitAiError::Generic(format!("Failed to open {:?}: {}", path, e)))
    }

    fn transcript_and_model_from_sqlite(
        db_path: &Path,
        session_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let conn = Self::open_sqlite_readonly(db_path)?;
        let messages = Self::read_session_messages_from_sqlite(&conn, session_id)?;

        if messages.is_empty() {
            return Ok((AiTranscript::new(), None));
        }

        Self::build_transcript_from_messages(messages, |message_id| {
            Self::read_message_parts_from_sqlite(&conn, session_id, message_id)
        })
    }

    fn transcript_and_model_from_legacy_storage(
        storage_path: &Path,
        session_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        if !storage_path.exists() {
            return Err(GitAiError::PresetError(format!(
                "OpenCode legacy storage path does not exist: {:?}",
                storage_path
            )));
        }

        let messages = Self::read_session_messages(storage_path, session_id)?;
        if messages.is_empty() {
            return Ok((AiTranscript::new(), None));
        }

        Self::build_transcript_from_messages(messages, |message_id| {
            Self::read_message_parts(storage_path, message_id)
        })
    }

    fn build_transcript_from_messages<F>(
        mut messages: Vec<TranscriptSourceMessage>,
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
                if let (Some(provider_id), Some(model_id)) =
                    (&message.provider_id, &message.model_id)
                {
                    model = Some(format!("{}/{}", provider_id, model_id));
                } else if let Some(model_id) = &message.model_id {
                    model = Some(model_id.clone());
                }
            }

            let parts = read_parts(&message.id)?;

            // Convert Unix ms to RFC3339 timestamp
            let timestamp =
                DateTime::from_timestamp_millis(message.created).map(|dt| dt.to_rfc3339());

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

    fn part_created_for_sort(part: &OpenCodePart, fallback: i64) -> i64 {
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
            OpenCodePart::StepStart { time, .. } => {
                time.as_ref().map(|t| t.start).unwrap_or(fallback)
            }
            OpenCodePart::StepFinish { time, .. } => {
                time.as_ref().map(|t| t.start).unwrap_or(fallback)
            }
            OpenCodePart::Unknown => fallback,
        }
    }

    /// Read all legacy message files for a session
    fn read_session_messages(
        storage_path: &Path,
        session_id: &str,
    ) -> Result<Vec<TranscriptSourceMessage>, GitAiError> {
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
                        Ok(message) => messages.push(TranscriptSourceMessage {
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

    /// Read all legacy part files for a message
    fn read_message_parts(
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
                            let created = Self::part_created_for_sort(&part, 0);
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

    fn read_session_messages_from_sqlite(
        conn: &Connection,
        session_id: &str,
    ) -> Result<Vec<TranscriptSourceMessage>, GitAiError> {
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
                    messages.push(TranscriptSourceMessage {
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

    fn read_message_parts_from_sqlite(
        conn: &Connection,
        session_id: &str,
        message_id: &str,
    ) -> Result<Vec<OpenCodePart>, GitAiError> {
        let mut stmt = conn
            .prepare(
                "SELECT id, time_created, data FROM part WHERE session_id = ? AND message_id = ? ORDER BY id ASC",
            )
            .map_err(|e| GitAiError::Generic(format!("SQLite query prepare failed: {}", e)))?;

        let mut rows = stmt
            .query([session_id, message_id])
            .map_err(|e| GitAiError::Generic(format!("SQLite query failed: {}", e)))?;

        let mut parts: Vec<(i64, OpenCodePart)> = Vec::new();

        while let Some(row) = rows
            .next()
            .map_err(|e| GitAiError::Generic(format!("SQLite row read failed: {}", e)))?
        {
            let part_id: String = row
                .get(0)
                .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;
            let created_column: i64 = row
                .get(1)
                .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;
            let data_text: String = row
                .get(2)
                .map_err(|e| GitAiError::Generic(format!("SQLite field read failed: {}", e)))?;

            match serde_json::from_str::<OpenCodePart>(&data_text) {
                Ok(part) => {
                    let created = Self::part_created_for_sort(&part, created_column);
                    parts.push((created, part));
                }
                Err(e) => {
                    eprintln!(
                        "[Warning] Failed to parse OpenCode sqlite part row {}: {}",
                        part_id, e
                    );
                }
            }
        }

        parts.sort_by_key(|(created, _)| *created);
        Ok(parts.into_iter().map(|(_, part)| part).collect())
    }
}
