use crate::streams::agent::{Agent, StreamDescriptor, get_all_agents};
use crate::streams::db::{StreamRecord, StreamsDatabase};
use crate::streams::sweep::{DiscoveredSession, SweepStrategy};
use crate::streams::types::StreamError;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

pub(crate) const MAX_SWEEP_ITEMS: usize = 4_096;

/// Work items discovered by the sweep.
#[derive(Debug, Clone)]
pub enum SweepItem {
    /// A session has at least one owned (non-shared) stream with new data.
    /// The worker should expand all streams for this session.
    Session {
        session_id: String,
        tool: String,
        canonical_path: PathBuf,
        external_session_id: String,
        external_parent_session_id: Option<String>,
    },
    /// A shared stream has new data. Process just this stream.
    SharedStream {
        tool: String,
        stream_kind: String,
        canonical_path: PathBuf,
    },
}

/// Orchestrates periodic sweeps across all registered agents.
///
/// Discovers sessions via each agent's filesystem scan, then checks staleness
/// separately for owned streams (per-session) and shared streams (once per agent).
pub struct SweepCoordinator {
    streams_db: Arc<StreamsDatabase>,
    agent_registry: Vec<(String, Box<dyn Agent>)>,
    lookback_days: Option<u32>,
}

impl SweepCoordinator {
    pub fn new(streams_db: Arc<StreamsDatabase>) -> Self {
        let lookback_days = crate::config::Config::get().transcript_streaming_lookback_days();
        Self {
            streams_db,
            agent_registry: get_all_agents(),
            lookback_days,
        }
    }

    #[cfg(test)]
    pub fn with_lookback(streams_db: Arc<StreamsDatabase>, lookback_days: Option<u32>) -> Self {
        Self {
            streams_db,
            agent_registry: get_all_agents(),
            lookback_days,
        }
    }

    /// Run a full sweep across all agents.
    pub fn run_sweep(&self) -> Result<Vec<SweepItem>, StreamError> {
        let mut items = Vec::new();

        for (agent_type, agent) in &self.agent_registry {
            if !matches!(agent.sweep_strategy(), SweepStrategy::Periodic(_)) {
                continue;
            }

            let remaining = MAX_SWEEP_ITEMS.saturating_sub(items.len());
            if remaining == 0 {
                break;
            }
            let discovery_limit =
                remaining.min(crate::streams::sweep::MAX_DISCOVERED_SESSIONS_PER_AGENT);
            let discovered = match agent.discover_sessions(discovery_limit) {
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

            let streams = agent.streams();
            let (shared, owned): (Vec<_>, Vec<_>) = streams.into_iter().partition(|s| s.shared);

            // Per-session: only check owned streams for staleness
            for session in &discovered {
                let canonical = Self::canonicalize_path(&session.stream_path);
                if self.any_stream_stale(session, &canonical, &owned)? {
                    items.push(SweepItem::Session {
                        session_id: session.session_id.clone(),
                        tool: session.tool.clone(),
                        canonical_path: canonical,
                        external_session_id: session.external_session_id.clone(),
                        external_parent_session_id: session.external_parent_session_id.clone(),
                    });
                    if items.len() == MAX_SWEEP_ITEMS {
                        break;
                    }
                }
            }

            // Shared streams: resolve from each distinct path (e.g., Code vs Code Insiders
            // have separate globalStorage directories with independent OTEL DBs)
            let mut seen_shared_paths = HashSet::new();
            for session in &discovered {
                let canonical = Self::canonicalize_path(&session.stream_path);
                for stream in &shared {
                    let Some(item) = self.check_shared_stream(stream, &canonical, &session.tool)?
                    else {
                        continue;
                    };
                    let SweepItem::SharedStream {
                        canonical_path: ref p,
                        ..
                    } = item
                    else {
                        continue;
                    };
                    if items.len() < MAX_SWEEP_ITEMS && seen_shared_paths.insert(p.clone()) {
                        items.push(item);
                    }
                }
            }
        }

        Ok(items)
    }

    /// Returns true if any owned (non-shared) stream file is new or has changed.
    fn any_stream_stale(
        &self,
        session: &DiscoveredSession,
        canonical_path: &Path,
        streams: &[StreamDescriptor],
    ) -> Result<bool, StreamError> {
        for stream in streams {
            let Some(path) = stream.resolve_path(canonical_path) else {
                continue;
            };
            if !path.exists() {
                continue;
            }

            let path_str = path.display().to_string();
            match self
                .streams_db
                .get_stream(&session.session_id, stream.stream_kind, &path_str)?
            {
                None => {
                    if !self.is_within_lookback(&path) {
                        continue;
                    }
                    return Ok(true);
                }
                Some(existing) => {
                    if Self::is_file_stale(&path, &existing)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// Check a single shared stream for processing.
    ///
    /// Shared streams (e.g., SQLite DBs in WAL mode) bypass file-metadata
    /// staleness checks because WAL writes don't update the main file's
    /// size/mtime. Instead, we always return them if the file exists —
    /// the watermark cursor inside the processing logic prevents re-processing.
    fn check_shared_stream(
        &self,
        stream: &StreamDescriptor,
        canonical_path: &Path,
        tool: &str,
    ) -> Result<Option<SweepItem>, StreamError> {
        let Some(path) = stream.resolve_path(canonical_path) else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }

        Ok(Some(SweepItem::SharedStream {
            tool: tool.to_string(),
            stream_kind: stream.stream_kind.to_string(),
            canonical_path: path,
        }))
    }

    fn is_within_lookback(&self, path: &Path) -> bool {
        let Some(days) = self.lookback_days else {
            return true;
        };
        let cutoff =
            SystemTime::now() - std::time::Duration::from_secs(u64::from(days) * 24 * 60 * 60);
        match std::fs::metadata(path) {
            Ok(meta) => meta.modified().map(|mtime| mtime >= cutoff).unwrap_or(true),
            Err(_) => true,
        }
    }

    fn is_file_stale(path: &Path, existing: &StreamRecord) -> Result<bool, StreamError> {
        let metadata = std::fs::metadata(path).map_err(|e| StreamError::Transient {
            message: format!("failed to stat {}: {}", path.display(), e),
            retry_after: std::time::Duration::from_secs(5),
        })?;
        let file_size = metadata.len() as i64;
        let modified = Self::get_modified_timestamp(&metadata);
        Ok(file_size != existing.last_known_size
            || (modified.is_some() && modified != existing.last_modified))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streams::agent::{PathResolverKind, StreamDescriptor};
    use crate::streams::sweep::StreamFormat;
    use crate::streams::types::StreamBatch;
    use crate::streams::watermark::{WatermarkStrategy, WatermarkType};

    struct ManySessionsAgent {
        path: PathBuf,
        count: usize,
    }

    impl Agent for ManySessionsAgent {
        fn sweep_strategy(&self) -> SweepStrategy {
            SweepStrategy::Periodic(std::time::Duration::from_secs(1))
        }

        fn discover_sessions(&self, _limit: usize) -> Result<Vec<DiscoveredSession>, StreamError> {
            Ok((0..self.count)
                .map(|index| DiscoveredSession {
                    session_id: format!("session-{index}"),
                    tool: "test".to_string(),
                    stream_path: self.path.clone(),
                    external_session_id: format!("external-{index}"),
                    external_parent_session_id: None,
                })
                .collect())
        }

        fn read_incremental(
            &self,
            _path: &Path,
            _watermark: Box<dyn WatermarkStrategy>,
            _session_id: &str,
        ) -> Result<StreamBatch, StreamError> {
            unreachable!("coordinator discovery does not read transcript contents")
        }

        fn extract_event_timestamp(
            &self,
            _event: &serde_json::Value,
            _file_meta: &std::fs::Metadata,
            _is_first_event: bool,
        ) -> u32 {
            0
        }

        fn streams(&self) -> Vec<StreamDescriptor> {
            vec![StreamDescriptor {
                stream_kind: "transcript",
                format: StreamFormat::ClaudeJsonl,
                watermark_type: WatermarkType::ByteOffset,
                path_resolver: PathResolverKind::Identity,
                shared: false,
                watermark_type_resolver: None,
                format_resolver: None,
            }]
        }
    }

    #[test]
    fn sweep_output_is_bounded_when_an_agent_discovers_many_stale_sessions() {
        const EXPECTED_SWEEP_LIMIT: usize = 4_096;

        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        let db = Arc::new(StreamsDatabase::open(temp.path().join("streams.db")).unwrap());
        let coordinator = SweepCoordinator {
            streams_db: db,
            agent_registry: vec![(
                "test".to_string(),
                Box::new(ManySessionsAgent {
                    path: transcript,
                    count: EXPECTED_SWEEP_LIMIT + 1,
                }),
            )],
            lookback_days: None,
        };

        let items = coordinator.run_sweep().unwrap();

        assert_eq!(items.len(), EXPECTED_SWEEP_LIMIT);
    }
}
