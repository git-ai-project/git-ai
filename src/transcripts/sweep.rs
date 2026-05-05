// src/transcripts/sweep.rs

use super::watermark::{WatermarkStrategy, WatermarkType};
use std::path::PathBuf;
use std::time::Duration;

/// Strategy for discovering new/updated sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepStrategy {
    /// Periodic polling at the given interval
    Periodic(Duration),
    /// File system watcher (not implemented yet)
    FsWatcher,
    /// HTTP API polling (not implemented yet)
    HttpApi,
    /// No sweep support for this agent
    None,
}

/// A session discovered during a sweep.
pub struct DiscoveredSession {
    pub session_id: String,
    pub agent_type: String,
    pub transcript_path: PathBuf,
    pub transcript_format: TranscriptFormat,
    pub watermark_type: WatermarkType,
    pub initial_watermark: Box<dyn WatermarkStrategy>,
    pub model: Option<String>,
    pub tool: Option<String>,
    pub external_thread_id: Option<String>,
}

impl std::fmt::Debug for DiscoveredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveredSession")
            .field("session_id", &self.session_id)
            .field("agent_type", &self.agent_type)
            .field("transcript_path", &self.transcript_path)
            .field("transcript_format", &self.transcript_format)
            .field("watermark_type", &self.watermark_type)
            .field("initial_watermark", &"<watermark>")
            .field("model", &self.model)
            .field("tool", &self.tool)
            .field("external_thread_id", &self.external_thread_id)
            .finish()
    }
}

impl Clone for DiscoveredSession {
    fn clone(&self) -> Self {
        // Clone the watermark by serializing and deserializing
        let serialized = self.initial_watermark.serialize();
        let cloned_watermark = self
            .watermark_type
            .deserialize(&serialized)
            .expect("Failed to clone watermark");

        Self {
            session_id: self.session_id.clone(),
            agent_type: self.agent_type.clone(),
            transcript_path: self.transcript_path.clone(),
            transcript_format: self.transcript_format,
            watermark_type: self.watermark_type,
            initial_watermark: cloned_watermark,
            model: self.model.clone(),
            tool: self.tool.clone(),
            external_thread_id: self.external_thread_id.clone(),
        }
    }
}

/// Transcript file format enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    ClaudeJsonl,
    CursorJsonl,
    DroidJsonl,
    CopilotSessionJson,
    CopilotEventStreamJsonl,
    GeminiJsonl,
    ContinueJson,
    WindsurfJsonl,
    CodexJsonl,
    AmpThreadJson,
    OpenCodeSqlite,
    PiJsonl,
}

impl std::fmt::Display for TranscriptFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeJsonl => write!(f, "ClaudeJsonl"),
            Self::CursorJsonl => write!(f, "CursorJsonl"),
            Self::DroidJsonl => write!(f, "DroidJsonl"),
            Self::CopilotSessionJson => write!(f, "CopilotSessionJson"),
            Self::CopilotEventStreamJsonl => write!(f, "CopilotEventStreamJsonl"),
            Self::GeminiJsonl => write!(f, "GeminiJsonl"),
            Self::ContinueJson => write!(f, "ContinueJson"),
            Self::WindsurfJsonl => write!(f, "WindsurfJsonl"),
            Self::CodexJsonl => write!(f, "CodexJsonl"),
            Self::AmpThreadJson => write!(f, "AmpThreadJson"),
            Self::OpenCodeSqlite => write!(f, "OpenCodeSqlite"),
            Self::PiJsonl => write!(f, "PiJsonl"),
        }
    }
}

impl TranscriptFormat {
    pub fn watermark_type(self) -> super::watermark::WatermarkType {
        use super::watermark::WatermarkType;
        match self {
            Self::ClaudeJsonl
            | Self::CursorJsonl
            | Self::GeminiJsonl
            | Self::WindsurfJsonl
            | Self::CodexJsonl
            | Self::PiJsonl
            | Self::CopilotEventStreamJsonl => WatermarkType::ByteOffset,
            Self::DroidJsonl => WatermarkType::Hybrid,
            Self::CopilotSessionJson | Self::ContinueJson | Self::AmpThreadJson => {
                WatermarkType::RecordIndex
            }
            Self::OpenCodeSqlite => WatermarkType::Timestamp,
        }
    }
}
