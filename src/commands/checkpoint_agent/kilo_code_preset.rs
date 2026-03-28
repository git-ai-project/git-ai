use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult,
    },
    error::GitAiError,
    observability::log_error,
};
use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct KiloCodePreset;

/// Hook input from Kilo Code plugin
#[derive(Debug, Deserialize)]
struct KiloCodeHookInput {
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

/// Message metadata from legacy file storage message/{session_id}/{msg_id}.json
#[derive(Debug, Deserialize)]
struct KiloCodeMessage {
    id: String,
    #[serde(rename = "sessionID", default)]
    #[allow(dead_code)]
    session_id: String,
    role: String, // "user" | "assistant"
    time: KiloCodeTime,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KiloCodeTime {
    created: i64,
    #[allow(dead_code)]
    completed: Option<i64>,
}

/// SQLite message payload from message.data
#[derive(Debug, Deserialize)]
struct KiloCodeDbMessageData {
    role: String,
    #[serde(default)]
    time: Option<KiloCodeTime>,
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
    time: Option<KiloCodePartTime>,
}

/// Part content from either legacy part/{msg_id}/{prt_id}.json or sqlite part.data
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
enum KiloCodePart {
    Text {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        text: String,
        time: Option<KiloCodePartTime>,
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
        time: Option<KiloCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepStart {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        #[allow(dead_code)]
        time: Option<KiloCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    StepFinish {
        #[serde(rename = "messageID", default)]
        #[allow(dead_code)]
        message_id: Option<String>,
        #[allow(dead_code)]
        time: Option<KiloCodePartTime>,
        #[allow(dead_code)]
        id: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct KiloCodePartTime {
    start: i64,
    #[allow(dead_code)]
    end: Option<i64>,
}

impl AgentCheckpointPreset for KiloCodePreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Kilo Code preset".to_string())
        })?;

        let hook_input: KiloCodeHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let KiloCodeHookInput {
            hook_event_name,
            session_id,
            cwd,
            tool_input,
        } = hook_input;

        // Extract file_path from tool_input if present
        let file_path_as_vec = tool_input
            .and_then(|ti| ti.file_path)
            .map(|path| vec![path]);

        // Determine Kilo Code path (test override can point to either root or legacy storage path)
        let kilo_code_path = if let Ok(test_path) = std::env::var("GIT_AI_KILO_CODE_STORAGE_PATH") {
            PathBuf::from(test_path)
        } else {
            Self::kilo_code_data_path()?
        };

        // Fetch transcript and model from sqlite first, then fallback to legacy storage
        let (transcript, model) =
            match Self::transcript_and_model_from_storage(&kilo_code_path, &session_id) {
                Ok((transcript, model)) => (transcript, model),
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Kilo Code storage: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "kilo-code",
                            "operation": "transcript_and_model_from_storage"
                        })),
                    );
                    (AiTranscript::new(), None)
                }
            };

        let agent_id = AgentId {
            tool: "kilo-code".to_string(),
            id: session_id.clone(),
            model: model.unwrap_or_else(|| "unknown".to_string()),
        };

        // Store session_id in metadata for post-commit refetch
        let mut agent_metadata = HashMap::new();
        agent_metadata.insert("session_id".to_string(), session_id);
        // Store test path if set, for subprocess access in tests
        if let Ok(test_path) = std::env::var("GIT_AI_KILO_CODE_STORAGE_PATH") {
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

impl KiloCodePreset {
    /// Get the Kilo Code data directory based on platform.
    /// Expected layout: {data_dir}/kilo.db and {data_dir}/storage
    pub fn kilo_code_data_path() -> Result<PathBuf, GitAiError> {
        #[cfg(target_os = "macos")]
        {
            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            Ok(home.join(".local").join("share").join("kilo"))
        }

        #[cfg(target_os = "linux")]
        {
            // Try XDG_DATA_HOME first, then fall back to ~/.local/share
            if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
                Ok(PathBuf::from(xdg_data).join("kilo"))
            } else {
                let home = dirs::home_dir().ok_or_else(|| {
                    GitAiError::Generic("Could not determine home directory".to_string())
                })?;
                Ok(home.join(".local").join("share").join("kilo"))
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Kilo Code uses ~/.local/share/kilo on all platforms (including Windows)
            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            let unix_style_path = home.join(".local").join("share").join("kilo");
            if unix_style_path.exists() {
                return Ok(unix_style_path);
            }

            // Fallback to standard Windows paths
            if let Ok(app_data) = std::env::var("APPDATA") {
                Ok(PathBuf::from(app_data).join("kilo"))
            } else if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                Ok(PathBuf::from(local_app_data).join("kilo"))
            } else {
                Err(GitAiError::Generic(
                    "Neither APPDATA nor LOCALAPPDATA is set".to_string(),
                ))
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(GitAiError::PresetError(
                "Kilo Code storage path not supported on this platform".to_string(),
            ))
        }
    }

    /// Public API for fetching transcript from session_id (uses default Kilo Code data path)
    pub fn transcript_and_model_from_session(
        session_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let kilo_code_path = Self::kilo_code_data_path()?;
        Self::transcript_and_model_from_storage(&kilo_code_path, session_id)
    }

    /// Fetch transcript and model from Kilo Code path (sqlite first, fallback to legacy storage)
    ///
    /// `kilo_code_path` may be one of:
    /// - Kilo Code data dir (contains `kilo.db` and optional `storage/`)
    /// - Legacy storage dir (contains `message/` and `part/`)
    /// - Direct path to `kilo.db`
    pub fn transcript_and_model_from_storage(
        kilo_code_path: &Path,
        session_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        if !kilo_code_path.exists() {
            return Err(GitAiError::PresetError(format!(
                "Kilo Code path does not exist: {:?}",
                kilo_code_path
            )));
        }

        let mut sqlite_empty_result: Option<(AiTranscript, Option<String>)> = None;
        let mut sqlite_error: Option<GitAiError> = None;

        if let Some(db_path) = Self::resolve_sqlite_db_path(kilo_code_path) {
            match Self::transcript_and_model_from_sqlite(&db_path, session_id) {
                Ok((transcript, model)) => {
                    if !transcript.messages().is_empty() || model.is_some() {
                        return Ok((transcript, model));
                    }
                    sqlite_empty_result = Some((transcript, model));
                }
                Err(e) => {
                    eprintln!(
                        "[Warning] Failed to parse Kilo Code sqlite db {:?}: {}",
                        db_path, e
                    );
                    sqlite_error = Some(e);
                }
            }
        }

        if let Some(storage_path) = Self::resolve_legacy_storage_path(kilo_code_path) {
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
            "No Kilo Code sqlite database or legacy storage found under {:?}",
            kilo_code_path
        )))
    }

    fn resolve_sqlite_db_path(path: &Path) -> Option<PathBuf> {
        if path.is_file() {
            return path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| *name == "kilo.db")
                .map(|_| path.to_path_buf());
        }

        if !path.is_dir() {
            return None;
        }

        let direct_db = path.join("kilo.db");
        if direct_db.exists() {
            return Some(direct_db);
        }

        // If caller passed legacy storage path, check sibling kilo.db
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "storage")
        {
            let sibling_db = path.parent()?.join("kilo.db");
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
                .is_some_and(|name| name == "kilo.db")
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
                "Kilo Code legacy storage path does not exist: {:?}",
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
        F: FnMut(&str) -> Result<Vec<KiloCodePart>, GitAiError>,
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
                    KiloCodePart::Text { text, .. } => {
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
                    KiloCodePart::Tool {
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
                    KiloCodePart::StepStart { .. } | KiloCodePart::StepFinish { .. } => {
                        // Skip step markers - they don't contribute to the transcript
                    }
                    KiloCodePart::Unknown => {
                        // Skip unknown part types
                    }
                }
            }
        }

        Ok((transcript, model))
    }

    fn part_created_for_sort(part: &KiloCodePart, fallback: i64) -> i64 {
        match part {
            KiloCodePart::Text { time, .. } => time.as_ref().map(|t| t.start).unwrap_or(fallback),
            KiloCodePart::Tool { time, state, .. } => time
                .as_ref()
                .map(|t| t.start)
                .or_else(|| {
                    state
                        .as_ref()
                        .and_then(|s| s.time.as_ref())
                        .map(|t| t.start)
                })
                .unwrap_or(fallback),
            KiloCodePart::StepStart { time, .. } => {
                time.as_ref().map(|t| t.start).unwrap_or(fallback)
            }
            KiloCodePart::StepFinish { time, .. } => {
                time.as_ref().map(|t| t.start).unwrap_or(fallback)
            }
            KiloCodePart::Unknown => fallback,
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
                    Ok(content) => match serde_json::from_str::<KiloCodeMessage>(&content) {
                        Ok(message) => messages.push(TranscriptSourceMessage {
                            id: message.id,
                            role: message.role,
                            created: message.time.created,
                            model_id: message.model_id,
                            provider_id: message.provider_id,
                        }),
                        Err(e) => {
                            eprintln!(
                                "[Warning] Failed to parse Kilo Code message file {:?}: {}",
                                path, e
                            );
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "[Warning] Failed to read Kilo Code message file {:?}: {}",
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
    ) -> Result<Vec<KiloCodePart>, GitAiError> {
        let part_dir = storage_path.join("part").join(message_id);
        if !part_dir.exists() {
            return Ok(Vec::new());
        }

        let mut parts: Vec<(i64, KiloCodePart)> = Vec::new();
        let entries = std::fs::read_dir(&part_dir).map_err(GitAiError::IoError)?;

        for entry in entries {
            let entry = entry.map_err(GitAiError::IoError)?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "json") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str::<KiloCodePart>(&content) {
                        Ok(part) => {
                            let created = Self::part_created_for_sort(&part, 0);
                            parts.push((created, part));
                        }
                        Err(e) => {
                            eprintln!(
                                "[Warning] Failed to parse Kilo Code part file {:?}: {}",
                                path, e
                            );
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "[Warning] Failed to read Kilo Code part file {:?}: {}",
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

            match serde_json::from_str::<KiloCodeDbMessageData>(&data_text) {
                Ok(data) => {
                    let KiloCodeDbMessageData {
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
                        "[Warning] Failed to parse Kilo Code sqlite message row {}: {}",
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
    ) -> Result<Vec<KiloCodePart>, GitAiError> {
        let mut stmt = conn
            .prepare(
                "SELECT id, time_created, data FROM part WHERE session_id = ? AND message_id = ? ORDER BY id ASC",
            )
            .map_err(|e| GitAiError::Generic(format!("SQLite query prepare failed: {}", e)))?;

        let mut rows = stmt
            .query([session_id, message_id])
            .map_err(|e| GitAiError::Generic(format!("SQLite query failed: {}", e)))?;

        let mut parts: Vec<(i64, KiloCodePart)> = Vec::new();

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

            match serde_json::from_str::<KiloCodePart>(&data_text) {
                Ok(part) => {
                    let created = Self::part_created_for_sort(&part, created_column);
                    parts.push((created, part));
                }
                Err(e) => {
                    eprintln!(
                        "[Warning] Failed to parse Kilo Code sqlite part row {}: {}",
                        part_id, e
                    );
                }
            }
        }

        parts.sort_by_key(|(created, _)| *created);
        Ok(parts.into_iter().map(|(_, part)| part).collect())
    }
}
