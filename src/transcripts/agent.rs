// src/transcripts/agent.rs

use super::sweep::{DiscoveredSession, SweepStrategy};
use super::types::{TranscriptBatch, TranscriptError};
use super::watermark::WatermarkStrategy;
use std::path::Path;

/// Unified trait for transcript agents.
///
/// Combines sweep discovery and incremental reading in one interface.
/// Agents that don't support sweeping return `SweepStrategy::None`.
pub trait Agent: Send + Sync {
    /// Returns the sweep strategy for this agent.
    fn sweep_strategy(&self) -> SweepStrategy;

    /// Discover all sessions in the agent's storage.
    ///
    /// Returns ALL sessions found, regardless of whether they're in transcripts-db.
    /// The coordinator will compare against the DB to decide what to process.
    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError>;

    /// Maximum number of events to return per `read_incremental` call.
    /// Bounds peak memory to batch_size × avg_event_size instead of file_size.
    /// The caller loops until an empty batch is returned.
    fn batch_size_hint(&self) -> usize {
        1000
    }

    /// Read transcript incrementally from the given watermark.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the transcript file
    /// * `watermark` - Current watermark position to resume from
    /// * `session_id` - Session ID for context (used in error messages)
    fn read_incremental(
        &self,
        path: &Path,
        watermark: Box<dyn WatermarkStrategy>,
        session_id: &str,
    ) -> Result<TranscriptBatch, TranscriptError>;
}

/// Get an agent implementation by type name.
///
/// Returns None for agents without sweep/read support (e.g., "human", "mock_ai").
pub fn get_agent(agent_type: &str) -> Option<Box<dyn Agent>> {
    match agent_type {
        "claude" => Some(Box::new(super::agents::ClaudeAgent)),
        "cursor" => Some(Box::new(super::agents::CursorAgent)),
        "droid" => Some(Box::new(super::agents::DroidAgent)),
        "copilot" => Some(Box::new(super::agents::CopilotAgent)),
        "gemini" => Some(Box::new(super::agents::GeminiAgent)),
        "continue-cli" => Some(Box::new(super::agents::ContinueAgent)),
        "windsurf" => Some(Box::new(super::agents::WindsurfAgent)),
        "codex" => Some(Box::new(super::agents::CodexAgent)),
        "amp" => Some(Box::new(super::agents::AmpAgent)),
        "opencode" => Some(Box::new(super::agents::OpenCodeAgent)),
        "pi" => Some(Box::new(super::agents::PiAgent)),
        _ => None,
    }
}
