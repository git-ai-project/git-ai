// src/daemon/sweep_coordinator.rs

use crate::transcripts::agent::{Agent, get_agent};
use crate::transcripts::db::{SessionRecord, TranscriptsDatabase};
use crate::transcripts::sweep::{DiscoveredSession, SweepStrategy};
use crate::transcripts::types::TranscriptError;
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;

/// Orchestrates periodic sweeps across all registered agents.
pub struct SweepCoordinator {
    transcripts_db: Arc<TranscriptsDatabase>,
    agent_registry: Vec<(String, Box<dyn Agent>)>,
}

impl SweepCoordinator {
    pub fn new(transcripts_db: Arc<TranscriptsDatabase>) -> Self {
        // Initialize with all agents that have sweep support
        let agent_registry = vec![
            (
                "claude".to_string(),
                get_agent("claude").expect("claude agent registered"),
            ),
            (
                "cursor".to_string(),
                get_agent("cursor").expect("cursor agent registered"),
            ),
            (
                "droid".to_string(),
                get_agent("droid").expect("droid agent registered"),
            ),
            (
                "copilot".to_string(),
                get_agent("copilot").expect("copilot agent registered"),
            ),
            (
                "codex".to_string(),
                get_agent("codex").expect("codex agent registered"),
            ),
        ];

        Self {
            transcripts_db,
            agent_registry,
        }
    }

    /// Run a full sweep across all agents.
    ///
    /// Returns sessions that need processing (new or behind).
    pub fn run_sweep(&self) -> Result<Vec<SessionToProcess>, TranscriptError> {
        let mut sessions_to_process = Vec::new();

        for (agent_type, agent) in &self.agent_registry {
            // Skip agents that don't support periodic sweeps
            if !matches!(agent.sweep_strategy(), SweepStrategy::Periodic(_)) {
                continue;
            }

            // Discover all sessions for this agent
            let discovered = match agent.discover_sessions() {
                Ok(sessions) => sessions,
                Err(e) => {
                    tracing::error!(
                        agent_type = %agent_type,
                        error = %e,
                        "agent discovery failed during sweep, skipping"
                    );
                    continue;
                }
            };

            for session in discovered {
                // Check against transcripts-db
                match self.transcripts_db.get_session(&session.session_id)? {
                    None => {
                        // New session - insert and queue for processing
                        self.insert_new_session(&session)?;
                        sessions_to_process.push(SessionToProcess {
                            session_id: session.session_id.clone(),
                            agent_type: session.agent_type.clone(),
                            canonical_path: Self::canonicalize_path(&session.transcript_path),
                        });
                    }
                    Some(existing) => {
                        // Session exists - check if it's behind
                        if self.is_session_behind(&session, &existing)? {
                            sessions_to_process.push(SessionToProcess {
                                session_id: session.session_id.clone(),
                                agent_type: session.agent_type.clone(),
                                canonical_path: Self::canonicalize_path(&session.transcript_path),
                            });
                        }
                    }
                }
            }
        }

        Ok(sessions_to_process)
    }

    fn is_session_behind(
        &self,
        discovered: &DiscoveredSession,
        existing: &SessionRecord,
    ) -> Result<bool, TranscriptError> {
        let metadata = std::fs::metadata(&discovered.transcript_path).map_err(|e| {
            TranscriptError::Transient {
                message: format!("failed to stat file: {}", e),
                retry_after: std::time::Duration::from_secs(5),
            }
        })?;

        let file_size = metadata.len() as i64;
        let modified = Self::get_modified_timestamp(&metadata);

        Ok(file_size != existing.last_known_size
            || (modified.is_some() && modified != existing.last_modified))
    }

    fn insert_new_session(&self, session: &DiscoveredSession) -> Result<(), TranscriptError> {
        let now = Utc::now().timestamp();
        let record = SessionRecord {
            session_id: session.session_id.clone(),
            agent_type: session.agent_type.clone(),
            transcript_path: session.transcript_path.display().to_string(),
            transcript_format: format!("{:?}", session.transcript_format),
            watermark_type: format!("{:?}", session.watermark_type),
            watermark_value: session.initial_watermark.serialize(),
            model: session.model.clone(),
            tool: session.tool.clone(),
            external_thread_id: session.external_thread_id.clone(),
            first_seen_at: now,
            last_processed_at: 0,
            last_known_size: 0,
            last_modified: None,
            processing_errors: 0,
            last_error: None,
        };

        self.transcripts_db.insert_session(&record)?;
        Ok(())
    }

    fn canonicalize_path(path: &PathBuf) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.clone())
    }

    fn get_modified_timestamp(metadata: &std::fs::Metadata) -> Option<i64> {
        metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    }
}

/// A session that needs processing.
#[derive(Debug, Clone)]
pub struct SessionToProcess {
    pub session_id: String,
    pub agent_type: String,
    pub canonical_path: PathBuf,
}
