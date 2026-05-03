pub mod parse;

mod agent_v1;
mod ai_tab;
mod amp;
mod claude;
mod codex;
mod continue_cli;
mod cursor;
mod droid;
mod firebender;
mod gemini;
mod github_copilot;
mod human;
mod known_human;
mod mock_ai;
mod mock_known_human;
mod opencode;
mod pi;
mod windsurf;

use crate::authorship::working_log::AgentId;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BashPreHookStrategy {
    EmitHumanCheckpoint,
    SnapshotOnly,
}
use crate::error::GitAiError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetContext {
    pub agent_id: AgentId,
    pub session_id: String,
    pub trace_id: String,
    pub cwd: PathBuf,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParsedHookEvent {
    PreFileEdit(PreFileEdit),
    PostFileEdit(PostFileEdit),
    PreBashCall(PreBashCall),
    PostBashCall(PostBashCall),
    KnownHumanEdit(KnownHumanEdit),
    UntrackedEdit(UntrackedEdit),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostFileEdit {
    pub context: PresetContext,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
    pub transcript_source: Option<TranscriptSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHumanEdit {
    pub trace_id: String,
    pub cwd: PathBuf,
    pub file_paths: Vec<PathBuf>,
    pub dirty_files: Option<HashMap<PathBuf, String>>,
    pub editor_metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrackedEdit {
    pub trace_id: String,
    pub cwd: PathBuf,
    pub file_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreBashCall {
    pub context: PresetContext,
    pub tool_use_id: String,
    pub strategy: BashPreHookStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostBashCall {
    pub context: PresetContext,
    pub tool_use_id: String,
    pub transcript_source: Option<TranscriptSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSource {
    pub path: PathBuf,
    pub format: TranscriptFormat,
    /// Session ID for this transcript (used to query/create session in DB).
    /// Defaults to empty string for backward compatibility.
    #[serde(default)]
    pub session_id: String,
    /// External thread/conversation ID (agent-specific identifier).
    #[serde(default)]
    pub external_thread_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptFormat {
    ClaudeJsonl,
    ContinueJson,
    GeminiJsonl,
    WindsurfJsonl,
    CodexJsonl,
    CursorJsonl,
    DroidJsonl,
    CopilotSessionJson,
    CopilotEventStreamJsonl,
    AmpThreadJson,
    OpenCodeSqlite,
    PiJsonl,
}

pub trait AgentPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError>;
}

pub fn resolve_preset(name: &str) -> Result<Box<dyn AgentPreset>, GitAiError> {
    match name {
        "claude" => Ok(Box::new(claude::ClaudePreset)),
        "codex" => Ok(Box::new(codex::CodexPreset)),
        "gemini" => Ok(Box::new(gemini::GeminiPreset)),
        "windsurf" => Ok(Box::new(windsurf::WindsurfPreset)),
        "continue-cli" => Ok(Box::new(continue_cli::ContinueCliPreset)),
        "cursor" => Ok(Box::new(cursor::CursorPreset)),
        "github-copilot" => Ok(Box::new(github_copilot::GithubCopilotPreset)),
        "amp" => Ok(Box::new(amp::AmpPreset)),
        "ai_tab" => Ok(Box::new(ai_tab::AiTabPreset)),
        "firebender" => Ok(Box::new(firebender::FirebenderPreset)),
        "agent-v1" => Ok(Box::new(agent_v1::AgentV1Preset)),
        "droid" => Ok(Box::new(droid::DroidPreset)),
        "opencode" => Ok(Box::new(opencode::OpenCodePreset)),
        "pi" => Ok(Box::new(pi::PiPreset)),
        "human" => Ok(Box::new(human::HumanPreset)),
        "mock_ai" => Ok(Box::new(mock_ai::MockAiPreset)),
        "known_human" => Ok(Box::new(known_human::KnownHumanPreset)),
        "mock_known_human" => Ok(Box::new(mock_known_human::MockKnownHumanPreset)),
        _ => Err(GitAiError::PresetError(format!("Unknown preset: {}", name))),
    }
}
