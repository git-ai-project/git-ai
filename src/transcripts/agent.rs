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
    /// Returns ALL sessions found, regardless of whether they're in transcripts.db.
    /// The coordinator will compare against the DB to decide what to process.
    fn discover_sessions(&self) -> Result<Vec<DiscoveredSession>, TranscriptError>;

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
        // More agents will be added as we implement them
        _ => None,
    }
}
