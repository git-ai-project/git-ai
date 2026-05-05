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

const ALL_AGENT_TYPES: &[&str] = &[
    "claude",
    "cursor",
    "droid",
    "copilot",
    "gemini",
    "continue-cli",
    "windsurf",
    "codex",
    "amp",
    "opencode",
    "pi",
];

/// Get an agent implementation by type name.
///
/// Returns None for agents without sweep/read support (e.g., "human", "mock_ai").
pub fn get_agent(agent_type: &str) -> Option<Box<dyn Agent>> {
    match agent_type {
        "claude" => Some(Box::new(super::agents::ClaudeAgent::new())),
        "cursor" => Some(Box::new(super::agents::CursorAgent::new())),
        "droid" => Some(Box::new(super::agents::DroidAgent::new())),
        "copilot" | "github-copilot" => Some(Box::new(super::agents::CopilotAgent::new())),
        "gemini" => Some(Box::new(super::agents::GeminiAgent::new())),
        "continue-cli" => Some(Box::new(super::agents::ContinueAgent::new())),
        "windsurf" => Some(Box::new(super::agents::WindsurfAgent::new())),
        "codex" => Some(Box::new(super::agents::CodexAgent::new())),
        "amp" => Some(Box::new(super::agents::AmpAgent::new())),
        "opencode" => Some(Box::new(super::agents::OpenCodeAgent::new())),
        "pi" => Some(Box::new(super::agents::PiAgent::new())),
        _ => None,
    }
}

/// Get all registered agents as (type_name, agent) pairs.
pub fn get_all_agents() -> Vec<(String, Box<dyn Agent>)> {
    ALL_AGENT_TYPES
        .iter()
        .filter_map(|&name| get_agent(name).map(|agent| (name.to_string(), agent)))
        .collect()
}
