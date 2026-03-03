use crate::{
    authorship::{
        transcript::{AiTranscript, Message},
        working_log::{AgentId, CheckpointKind},
    },
    commands::checkpoint_agent::agent_presets::{
        AgentCheckpointFlags, AgentCheckpointPreset, AgentRunResult, CheckpointExecution,
    },
    error::GitAiError,
    observability::log_error,
};
use chrono::DateTime;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct AmpPreset;

#[derive(Debug, Deserialize)]
struct AmpHookInput {
    hook_event_name: String,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    edited_filepaths: Option<Vec<String>>,
    #[serde(default)]
    tool_input: Option<serde_json::Value>,
}

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

impl AgentCheckpointPreset for AmpPreset {
    fn run(&self, flags: AgentCheckpointFlags) -> Result<AgentRunResult, GitAiError> {
        let hook_input_json = flags.hook_input.ok_or_else(|| {
            GitAiError::PresetError("hook_input is required for Amp preset".to_string())
        })?;

        let hook_input: AmpHookInput = serde_json::from_str(&hook_input_json)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let is_pre_tool_use = hook_input.hook_event_name == "PreToolUse";

        let file_paths = Self::extract_file_paths(&hook_input);
        let resolved_thread_path = Self::resolve_thread_path(
            hook_input.transcript_path.as_deref(),
            hook_input.thread_id.as_deref(),
            hook_input.tool_use_id.as_deref(),
        )?;

        let mut transcript = AiTranscript::new();
        let mut model: Option<String> = None;
        let mut resolved_thread_id = hook_input.thread_id.clone();

        if let Some(thread_path) = &resolved_thread_path {
            match Self::transcript_and_model_from_thread_path(thread_path) {
                Ok((parsed_transcript, parsed_model, parsed_thread_id)) => {
                    transcript = parsed_transcript;
                    model = parsed_model;
                    resolved_thread_id = Some(parsed_thread_id);
                }
                Err(e) => {
                    eprintln!("[Warning] Failed to parse Amp thread file: {e}");
                    log_error(
                        &e,
                        Some(serde_json::json!({
                            "agent_tool": "amp",
                            "operation": "transcript_and_model_from_thread_path"
                        })),
                    );
                }
            }
        }

        let agent_id = AgentId {
            tool: "amp".to_string(),
            id: resolved_thread_id
                .or(hook_input.tool_use_id.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            model: model.unwrap_or_else(|| "unknown".to_string()),
        };

        if is_pre_tool_use {
            return Ok(AgentRunResult {
                agent_id,
                agent_metadata: None,
                checkpoint_kind: CheckpointKind::Human,
                transcript: None,
                repo_working_dir: hook_input.cwd,
                edited_filepaths: None,
                will_edit_filepaths: file_paths,
                dirty_files: None,
                checkpoint_execution: CheckpointExecution::Run,
                telemetry_events: Vec::new(),
            });
        }

        let mut agent_metadata = HashMap::new();
        if let Some(path) = resolved_thread_path {
            agent_metadata.insert(
                "transcript_path".to_string(),
                path.to_string_lossy().to_string(),
            );
        }
        if let Some(thread_id) = hook_input.thread_id.or_else(|| {
            agent_metadata
                .get("transcript_path")
                .and_then(|p| Path::new(p).file_stem())
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        }) {
            agent_metadata.insert("thread_id".to_string(), thread_id);
        }

        Ok(AgentRunResult {
            agent_id,
            agent_metadata: if agent_metadata.is_empty() {
                None
            } else {
                Some(agent_metadata)
            },
            checkpoint_kind: CheckpointKind::AiAgent,
            transcript: Some(transcript),
            repo_working_dir: hook_input.cwd,
            edited_filepaths: file_paths,
            will_edit_filepaths: None,
            dirty_files: None,
            checkpoint_execution: CheckpointExecution::Run,
            telemetry_events: Vec::new(),
        })
    }
}

impl AmpPreset {
    /// Get the default Amp threads directory based on platform.
    /// Expected layout: `{threads_dir}/T-THREAD_ID.json`
    pub fn amp_threads_path() -> Result<PathBuf, GitAiError> {
        if let Ok(test_path) = std::env::var("GIT_AI_AMP_THREADS_PATH") {
            return Ok(PathBuf::from(test_path));
        }

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
                return Ok(PathBuf::from(xdg_data).join("amp").join("threads"));
            }

            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
            Ok(home
                .join(".local")
                .join("share")
                .join("amp")
                .join("threads"))
        }

        #[cfg(target_os = "windows")]
        {
            if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                return Ok(PathBuf::from(local_app_data).join("amp").join("threads"));
            }
            if let Ok(app_data) = std::env::var("APPDATA") {
                return Ok(PathBuf::from(app_data).join("amp").join("threads"));
            }

            let home = dirs::home_dir().ok_or_else(|| {
                GitAiError::Generic("Could not determine home directory".to_string())
            })?;
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

    pub fn transcript_and_model_from_thread_id(
        thread_id: &str,
    ) -> Result<(AiTranscript, Option<String>), GitAiError> {
        let thread_path = Self::amp_threads_path()?.join(format!("{}.json", thread_id));
        let (transcript, model, _resolved_thread_id) =
            Self::transcript_and_model_from_thread_path(&thread_path)?;
        Ok((transcript, model))
    }

    pub fn transcript_and_model_from_thread_path(
        thread_path: &Path,
    ) -> Result<(AiTranscript, Option<String>, String), GitAiError> {
        let content = std::fs::read_to_string(thread_path)
            .map_err(|e| GitAiError::Generic(format!("Failed to read thread file: {}", e)))?;

        let thread: AmpThread = serde_json::from_str(&content)
            .map_err(|e| GitAiError::Generic(format!("Failed to parse Amp thread JSON: {}", e)))?;

        let (transcript, model) = Self::transcript_and_model_from_thread(&thread);

        Ok((transcript, model, thread.id))
    }

    fn transcript_and_model_from_thread(thread: &AmpThread) -> (AiTranscript, Option<String>) {
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

    fn resolve_thread_path(
        transcript_path: Option<&str>,
        thread_id: Option<&str>,
        tool_use_id: Option<&str>,
    ) -> Result<Option<PathBuf>, GitAiError> {
        if let Some(path) = transcript_path {
            let path = PathBuf::from(path);
            if path.exists() {
                return Ok(Some(path));
            }
        }

        let threads_path = Self::amp_threads_path()?;

        if threads_path.is_file()
            && threads_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            return Ok(Some(threads_path));
        }

        if let Some(thread_id) = thread_id {
            let candidate = threads_path.join(format!("{}.json", thread_id));
            if candidate.exists() {
                return Ok(Some(candidate));
            }
        }

        if let Some(tool_use_id) = tool_use_id {
            return Self::find_thread_file_by_tool_use_id(&threads_path, tool_use_id);
        }

        Ok(None)
    }

    fn find_thread_file_by_tool_use_id(
        threads_path: &Path,
        tool_use_id: &str,
    ) -> Result<Option<PathBuf>, GitAiError> {
        if !threads_path.exists() {
            return Ok(None);
        }

        let mut newest_match: Option<(PathBuf, std::time::SystemTime)> = None;

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

            if !Self::thread_file_contains_tool_use_id(&path, tool_use_id)? {
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

    fn thread_file_contains_tool_use_id(
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

    fn extract_file_paths(hook_input: &AmpHookInput) -> Option<Vec<String>> {
        if let Some(paths) = hook_input.edited_filepaths.clone()
            && !paths.is_empty()
        {
            return Some(paths);
        }

        let tool_input = hook_input.tool_input.as_ref()?;
        let mut files = Vec::new();

        for key in ["path", "filePath", "file_path"] {
            if let Some(path) = tool_input.get(key).and_then(|value| value.as_str())
                && !path.trim().is_empty()
            {
                files.push(path.to_string());
            }
        }

        if let Some(paths) = tool_input.get("paths").and_then(|value| value.as_array()) {
            for path in paths {
                if let Some(path) = path.as_str()
                    && !path.trim().is_empty()
                {
                    files.push(path.to_string());
                }
            }
        }

        if files.is_empty() { None } else { Some(files) }
    }
}
