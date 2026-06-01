//! Daemon-side transcript worker for sweep-based transcript discovery.
//!
//! Runs inside the daemon process with two event sources:
//! 1. **Checkpoint notifications** (Immediate priority, <100ms) - fired when `git-ai checkpoint` is called
//! 2. **Periodic sweeps** (Low priority, every 30min) - agent-specific discovery of all sessions

use crate::authorship::authorship_log_serialization::{generate_session_id, generate_trace_id};
use crate::config;
use crate::daemon::telemetry_worker::DaemonTelemetryWorkerHandle;
use crate::daemon::transcript_redaction::redact_json_secrets;
use crate::metrics::{EventAttributes, MetricEvent, PosEncoded, SessionEventValues};
use crate::transcripts::db::TranscriptsDatabase;
use crate::transcripts::types::TranscriptError;
use crate::transcripts::watermark::WatermarkType;
use chrono::{TimeZone, Utc};
use std::collections::{BinaryHeap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{Duration, interval};

const PROCESSING_TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Extract a Unix-epoch u32 timestamp from a raw JSON event's "timestamp" field.
/// Handles both ISO 8601 strings (e.g. "2026-05-11T23:13:12.819Z") and numeric
/// milliseconds (e.g. 1759845073835). Returns None if the field is missing or unparseable.
pub fn extract_event_timestamp(event: &serde_json::Value) -> Option<u32> {
    let ts_val = event.get("timestamp")?;
    if let Some(s) = ts_val.as_str() {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp() as u32)
    } else {
        ts_val.as_u64().map(|ms| (ms / 1000) as u32)
    }
}

/// Priority levels for processing tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(super) enum Priority {
    Low = 2, // Sweep-discovered sessions
    Immediate = 0, // Checkpoint-triggered, process first
             // REMOVED: High = 1 (was polling)
}

/// Task to process a session's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
pub(super) struct ProcessingTask {
    pub(super) priority: Priority,
    pub(super) session_id: String,
    pub(super) tool: String,
    pub(super) trace_id: Option<String>,
    pub(super) tool_use_id: Option<String>,
    pub(super) canonical_path: PathBuf,
    pub(super) repo_work_dir: Option<PathBuf>,
    pub(super) retry_count: u32,
    #[cfg_attr(test, serde(skip))]
    pub(super) next_retry_at: Option<std::time::Instant>,
}

impl PartialOrd for ProcessingTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProcessingTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher priority first (Immediate=0 < High=1 < Low=2)
        // Reverse comparison so smaller numeric value = higher priority = popped first from max-heap
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| self.session_id.cmp(&other.session_id))
    }
}

/// Handle for sending checkpoint notifications to the worker.
#[derive(Clone)]
pub struct TranscriptWorkerHandle {
    checkpoint_tx: tokio::sync::mpsc::UnboundedSender<CheckpointNotification>,
}

impl TranscriptWorkerHandle {
    /// Notify the worker that a checkpoint was recorded.
    pub fn notify_checkpoint(
        &self,
        session_id: String,
        tool: String,
        trace_id: String,
        tool_use_id: Option<String>,
        transcript_path: PathBuf,
        repo_work_dir: Option<PathBuf>,
    ) {
        let notification = CheckpointNotification {
            session_id,
            tool,
            trace_id,
            tool_use_id,
            transcript_path,
            repo_work_dir,
        };
        let _ = self.checkpoint_tx.send(notification);
    }
}

#[derive(Debug, Clone)]
struct CheckpointNotification {
    session_id: String,
    tool: String,
    trace_id: String,
    tool_use_id: Option<String>,
    transcript_path: PathBuf,
    repo_work_dir: Option<PathBuf>,
}

/// Worker that processes transcript changes.
struct TranscriptWorker {
    transcripts_db: Arc<TranscriptsDatabase>,
    sweep_coordinator: crate::daemon::sweep_coordinator::SweepCoordinator, // NEW
    priority_queue: BinaryHeap<ProcessingTask>,
    delayed_tasks: Vec<ProcessingTask>,
    in_flight: HashSet<PathBuf>,
    telemetry_handle: DaemonTelemetryWorkerHandle,
    shutdown_notify: Arc<Notify>,
    checkpoint_rx: tokio::sync::mpsc::UnboundedReceiver<CheckpointNotification>,
}

impl TranscriptWorker {
    /// Create a new transcript worker.
    fn new(
        transcripts_db: Arc<TranscriptsDatabase>,
        telemetry_handle: DaemonTelemetryWorkerHandle,
        shutdown_notify: Arc<Notify>,
        checkpoint_rx: tokio::sync::mpsc::UnboundedReceiver<CheckpointNotification>,
    ) -> Self {
        let sweep_coordinator =
            crate::daemon::sweep_coordinator::SweepCoordinator::new(transcripts_db.clone());

        Self {
            transcripts_db,
            sweep_coordinator, // NEW
            priority_queue: BinaryHeap::new(),
            delayed_tasks: Vec::new(),
            in_flight: HashSet::new(),
            telemetry_handle,
            shutdown_notify,
            checkpoint_rx,
        }
    }

    /// Main processing loop.
    async fn run(mut self) {
        tracing::info!("transcript worker started");

        let sweep_enabled = config::Config::get().get_feature_flags().transcript_sweep;

        let mut processing_ticker = interval(PROCESSING_TICK_INTERVAL);
        let mut sweep_ticker = interval(Duration::from_secs(30 * 60)); // NEW: 30 minutes

        // Skip the first immediate tick
        processing_ticker.tick().await;
        sweep_ticker.tick().await;

        // Run initial sweep on startup
        if sweep_enabled && let Err(e) = self.run_sweep().await {
            tracing::error!(error = %e, "initial sweep failed");
        }

        loop {
            tokio::select! {
                _ = self.shutdown_notify.notified() => {
                    tracing::info!("transcript worker received shutdown signal");
                    self.drain_immediate_tasks().await;
                    break;
                }
                _ = processing_ticker.tick() => {
                    self.process_next_task().await;
                }
                _ = sweep_ticker.tick() => {  // NEW: sweep ticker
                    if sweep_enabled
                        && let Err(e) = self.run_sweep().await
                    {
                        tracing::error!(error = %e, "sweep failed");
                    }
                }
                Some(notification) = self.checkpoint_rx.recv() => {
                    self.handle_checkpoint_notification(notification).await;
                }
            }
        }

        tracing::info!("transcript worker shutdown complete");
    }

    /// Run a sweep across all agents to discover new/behind sessions.
    async fn run_sweep(&mut self) -> Result<(), String> {
        let sessions = self
            .sweep_coordinator
            .run_sweep()
            .map_err(|e| e.to_string())?;

        tracing::info!(discovered = sessions.len(), "sweep completed");

        for session in sessions {
            // Deduplicate via in_flight
            if self.in_flight.contains(&session.canonical_path) {
                continue;
            }

            self.priority_queue.push(ProcessingTask {
                priority: Priority::Low,
                session_id: session.session_id,
                tool: session.tool,
                trace_id: None,
                tool_use_id: None,
                canonical_path: session.canonical_path,
                repo_work_dir: None,
                retry_count: 0,
                next_retry_at: None,
            });
        }

        Ok(())
    }

    /// Handle a checkpoint notification.
    async fn handle_checkpoint_notification(&mut self, notification: CheckpointNotification) {
        let canonical_path = std::fs::canonicalize(&notification.transcript_path)
            .unwrap_or_else(|_| notification.transcript_path.clone());

        // Deduplicate via in_flight
        if !self.in_flight.contains(&canonical_path) {
            self.priority_queue.push(ProcessingTask {
                priority: Priority::Immediate,
                session_id: notification.session_id.clone(),
                tool: notification.tool.clone(),
                trace_id: Some(notification.trace_id.clone()),
                tool_use_id: notification.tool_use_id.clone(),
                canonical_path,
                repo_work_dir: notification.repo_work_dir.clone(),
                retry_count: 0,
                next_retry_at: None,
            });
        }

        // Sweep subagent transcripts for this main session (Claude only for now)
        if notification.tool == "claude" {
            self.sweep_subagents_for_session(&notification);
        }
    }

    /// Discover and enqueue subagent transcripts belonging to a main Claude session.
    ///
    /// Given a main session at `<project>/<uuid>.jsonl`, subagents live at
    /// `<project>/<uuid>/subagents/agent-*.jsonl`.
    fn sweep_subagents_for_session(&mut self, notification: &CheckpointNotification) {
        let transcript_path = &notification.transcript_path;

        let subagents_dir = match transcript_path.file_stem() {
            Some(stem) => transcript_path.with_file_name(stem).join("subagents"),
            None => return,
        };

        if !subagents_dir.is_dir() {
            return;
        }

        let Ok(entries) = std::fs::read_dir(&subagents_dir) else {
            return;
        };

        let external_parent_session_id = transcript_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().map(|ext| ext == "jsonl") != Some(true) {
                continue;
            }

            let Some(external_session_id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
            else {
                continue;
            };

            let session_id = generate_session_id(&external_session_id, "claude");

            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if self.in_flight.contains(&canonical) {
                continue;
            }

            // Ensure the subagent session exists in the DB
            if let Err(e) = self.ensure_subagent_session(
                &session_id,
                &path,
                &external_session_id,
                external_parent_session_id.as_deref(),
                notification.repo_work_dir.as_deref(),
            ) {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "failed to ensure subagent session exists"
                );
                continue;
            }

            self.priority_queue.push(ProcessingTask {
                priority: Priority::Low,
                session_id,
                tool: "claude".to_string(),
                trace_id: Some(notification.trace_id.clone()),
                tool_use_id: None,
                canonical_path: canonical,
                repo_work_dir: notification.repo_work_dir.clone(),
                retry_count: 0,
                next_retry_at: None,
            });
        }
    }

    fn ensure_subagent_session(
        &self,
        session_id: &str,
        path: &Path,
        external_session_id: &str,
        external_parent_session_id: Option<&str>,
        repo_work_dir: Option<&Path>,
    ) -> Result<(), TranscriptError> {
        if self.transcripts_db.get_session(session_id)?.is_some() {
            return Ok(());
        }

        use crate::transcripts::db::SessionRecord;
        use crate::transcripts::watermark::{ByteOffsetWatermark, WatermarkStrategy};

        let initial_watermark = ByteOffsetWatermark::new(0);
        let record = SessionRecord {
            session_id: session_id.to_string(),
            tool: "claude".to_string(),
            transcript_path: path.display().to_string(),
            transcript_format: "ClaudeJsonl".to_string(),
            watermark_type: "ByteOffset".to_string(),
            watermark_value: initial_watermark.serialize(),
            external_session_id: external_session_id.to_string(),
            external_parent_session_id: external_parent_session_id.map(|s| s.to_string()),
            first_seen_at: chrono::Utc::now().timestamp(),
            last_processed_at: 0,
            last_known_size: 0,
            last_modified: None,
            processing_errors: 0,
            last_error: None,
            repo_work_dir: repo_work_dir.map(|p| p.display().to_string()),
        };

        self.transcripts_db.insert_session(&record)
    }

    /// Process the next task from the queue.
    async fn process_next_task(&mut self) {
        // Move any now-ready delayed tasks back to the priority queue
        let now = std::time::Instant::now();
        let mut i = 0;
        while i < self.delayed_tasks.len() {
            if self.delayed_tasks[i].next_retry_at.is_none_or(|t| now >= t) {
                let task = self.delayed_tasks.swap_remove(i);
                self.priority_queue.push(task);
            } else {
                i += 1;
            }
        }

        let Some(task) = self.priority_queue.pop() else {
            return;
        };

        // Check if task is ready to be processed (retry delay)
        if let Some(next_retry_at) = task.next_retry_at
            && now < next_retry_at
        {
            self.delayed_tasks.push(task);
            return;
        }

        // Mark as in-flight
        self.in_flight.insert(task.canonical_path.clone());

        // Process the session (spawn blocking to avoid blocking the worker loop)
        let db = self.transcripts_db.clone();
        let telemetry = self.telemetry_handle.clone();
        let task_clone = task.clone();

        let result = tokio::task::spawn_blocking(move || {
            Self::process_session_blocking(&db, &telemetry, &task_clone)
        })
        .await;

        // Remove from in-flight
        self.in_flight.remove(&task.canonical_path);

        // Handle result
        match result {
            Ok(Ok(())) => {
                // Success - task is done
            }
            Ok(Err(e)) => {
                // Error - handle retry logic
                self.handle_processing_error(task, e).await;
            }
            Err(e) => {
                // Panic in spawn_blocking
                tracing::error!(error = %e, session_id = %task.session_id, "task panicked");
                self.handle_processing_error(
                    task,
                    TranscriptError::Fatal {
                        message: format!("task panicked: {}", e),
                    },
                )
                .await;
            }
        }
    }

    /// Process a session (blocking I/O).
    ///
    /// Loops over bounded batches from `read_incremental`, saving the watermark
    /// after each batch for crash resilience. Applies backpressure between
    /// batches when the telemetry buffer is above a threshold, sleeping to let
    /// the 3-second flush cycle drain it.
    fn process_session_blocking(
        db: &TranscriptsDatabase,
        telemetry: &DaemonTelemetryWorkerHandle,
        task: &ProcessingTask,
    ) -> Result<(), TranscriptError> {
        let session = db
            .get_session(&task.session_id)?
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!("session not found: {}", task.session_id),
            })?;

        let agent = crate::transcripts::agent::get_agent(&task.tool).ok_or_else(|| {
            TranscriptError::Fatal {
                message: format!("unknown agent type: {}", task.tool),
            }
        })?;

        let watermark_type: WatermarkType = session.watermark_type.parse()?;

        let mut current_watermark = watermark_type.deserialize(&session.watermark_value)?;
        let path = PathBuf::from(&session.transcript_path);
        let mut total_events = 0usize;
        let parent_session_id = session
            .external_parent_session_id
            .as_ref()
            .map(|ext_pid| generate_session_id(ext_pid, &session.tool));

        // Resolve repo_work_dir with priority: task (hook) > DB > infer from transcript
        let resolved_work_dir = task
            .repo_work_dir
            .clone()
            .or_else(|| session.repo_work_dir.as_ref().map(PathBuf::from))
            .or_else(|| agent.infer_cwd(&path));

        // Persist inferred cwd to DB if session didn't already have one
        if session.repo_work_dir.is_none()
            && let Some(ref work_dir) = resolved_work_dir
        {
            let _ = db.update_repo_work_dir(&session.session_id, &work_dir.display().to_string());
        }

        let mut base_attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
            .session_id(session.session_id.clone())
            .tool(&session.tool)
            .external_session_id(session.external_session_id.clone())
            .external_parent_session_id_opt(session.external_parent_session_id.clone())
            .parent_session_id_opt(parent_session_id);

        if let Some(ref work_dir) = resolved_work_dir
            && let Some(url) = crate::repo_url::resolve_repo_url_from_path(work_dir)
        {
            base_attrs = base_attrs.repo_url(url);
        }

        let file_meta = std::fs::metadata(&path).ok();
        let is_initial_watermark = session.watermark_value.is_empty()
            || session.watermark_value == "0"
            || session.watermark_value == "0|0|"
            || session.watermark_value == "1970-01-01T00:00:00+00:00";

        loop {
            let batch = agent.read_incremental(&path, current_watermark, &session.session_id)?;

            if batch.events.is_empty() {
                db.update_watermark(&session.session_id, batch.new_watermark.as_ref())?;
                break;
            }

            let batch_count = batch.events.len();

            let metric_events: Vec<MetricEvent> = batch
                .events
                .into_iter()
                .enumerate()
                .map(|(idx, raw_event)| {
                    let (eid, pid, tid) = agent.extract_event_ids(&raw_event);
                    let is_first_event = is_initial_watermark && total_events == 0 && idx == 0;
                    let event_ts = match &file_meta {
                        Some(meta) => {
                            agent.extract_event_timestamp(&raw_event, meta, is_first_event)
                        }
                        None => extract_event_timestamp(&raw_event).unwrap_or_else(|| {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as u32
                        }),
                    };
                    let trace_id = generate_trace_id();
                    let attrs_sparse = base_attrs.clone().trace_id(trace_id).to_sparse();
                    let raw_event = redact_json_secrets(raw_event);
                    MetricEvent::from_values_with_timestamp(
                        SessionEventValues::with_ids(raw_event, eid, pid, tid),
                        attrs_sparse,
                        Some(event_ts),
                    )
                })
                .collect();

            crate::observability::log_metrics(metric_events);

            // Backpressure: if the telemetry buffer has accumulated too many
            // events, poll briefly to let the 3-second flush cycle drain it.
            // Short sleeps (~100ms) keep shutdown latency low since this runs
            // inside spawn_blocking. Capped at ~4s to avoid blocking forever
            // if the flush loop is stuck (API down, etc.).
            const BACKPRESSURE_THRESHOLD: usize = 5_000;
            const BACKPRESSURE_MAX_WAITS: usize = 40;
            for _ in 0..BACKPRESSURE_MAX_WAITS {
                if telemetry.metrics_buffer_len() < BACKPRESSURE_THRESHOLD {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            total_events += batch_count;
            db.update_watermark(&session.session_id, batch.new_watermark.as_ref())?;
            current_watermark = batch.new_watermark;
        }

        if let Ok(metadata) = std::fs::metadata(&session.transcript_path) {
            let file_size = metadata.len();
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| Utc.timestamp_opt(d.as_secs() as i64, 0).unwrap());
            db.update_file_metadata(&session.session_id, file_size, modified)?;
        }

        tracing::debug!(
            session_id = %task.session_id,
            events = total_events,
            "processed session"
        );

        Ok(())
    }

    /// Handle a processing error with exponential backoff.
    async fn handle_processing_error(&mut self, task: ProcessingTask, error: TranscriptError) {
        match error {
            TranscriptError::Transient { message, .. } => {
                // Retry with exponential backoff: 5s, 30s, 5m, 30m
                let retry_count = task.retry_count + 1;
                let max_retries = 4;

                if retry_count >= max_retries {
                    tracing::error!(
                        session_id = %task.session_id,
                        error = %message,
                        "max retries exceeded, dropping task"
                    );
                    if let Err(e) = self
                        .transcripts_db
                        .record_error(&task.session_id, &format!("max retries: {}", message))
                    {
                        tracing::warn!(session_id = %task.session_id, error = %e, "failed to record error in database");
                    }
                    return;
                }

                let delay = match retry_count {
                    1 => Duration::from_secs(5),
                    2 => Duration::from_secs(30),
                    3 => Duration::from_secs(5 * 60),
                    _ => Duration::from_secs(30 * 60),
                };

                tracing::warn!(
                    session_id = %task.session_id,
                    error = %message,
                    retry = retry_count,
                    delay_secs = delay.as_secs(),
                    "transient error, will retry"
                );

                // Re-queue with updated retry count and next_retry_at
                let mut retried_task = task.clone();
                retried_task.retry_count = retry_count;
                retried_task.next_retry_at = Some(std::time::Instant::now() + delay);
                self.priority_queue.push(retried_task);
            }
            TranscriptError::Parse { line, message } => {
                // Parse errors are not retried
                tracing::error!(
                    session_id = %task.session_id,
                    line = line,
                    error = %message,
                    "parse error, skipping session"
                );
                if let Err(e) = self.transcripts_db.record_error(
                    &task.session_id,
                    &format!("parse line {}: {}", line, message),
                ) {
                    tracing::warn!(session_id = %task.session_id, error = %e, "failed to record error in database");
                }
            }
            TranscriptError::Fatal { message } => {
                // Fatal errors are not retried
                tracing::error!(
                    session_id = %task.session_id,
                    error = %message,
                    "fatal error, skipping session"
                );
                if let Err(e) = self
                    .transcripts_db
                    .record_error(&task.session_id, &format!("fatal: {}", message))
                {
                    tracing::warn!(session_id = %task.session_id, error = %e, "failed to record error in database");
                }
            }
        }
    }

    /// Drain immediate priority tasks before shutdown.
    async fn drain_immediate_tasks(&mut self) {
        let mut immediate_tasks = Vec::new();

        // Collect all immediate tasks from priority queue and delayed tasks
        while let Some(task) = self.priority_queue.pop() {
            if task.priority == Priority::Immediate {
                immediate_tasks.push(task);
            }
        }
        let mut i = 0;
        while i < self.delayed_tasks.len() {
            if self.delayed_tasks[i].priority == Priority::Immediate {
                immediate_tasks.push(self.delayed_tasks.swap_remove(i));
            } else {
                i += 1;
            }
        }

        tracing::info!(tasks = immediate_tasks.len(), "draining immediate tasks");

        // Process immediate tasks
        for task in immediate_tasks {
            self.in_flight.insert(task.canonical_path.clone());
            let db = self.transcripts_db.clone();
            let telemetry = self.telemetry_handle.clone();
            let task_clone = task.clone();

            let result = tokio::task::spawn_blocking(move || {
                Self::process_session_blocking(&db, &telemetry, &task_clone)
            })
            .await;

            self.in_flight.remove(&task.canonical_path);

            match result {
                Err(e) => {
                    tracing::error!(error = %e, session_id = %task.session_id, "drain task panicked");
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, session_id = %task.session_id, "drain task processing error");
                }
                Ok(Ok(())) => {}
            }
        }
    }
}

/// Spawn the transcript worker.
pub fn spawn_transcript_worker(
    transcripts_db: Arc<TranscriptsDatabase>,
    telemetry_handle: DaemonTelemetryWorkerHandle,
    shutdown_notify: Arc<Notify>,
) -> TranscriptWorkerHandle {
    let (checkpoint_tx, checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();

    let worker = TranscriptWorker::new(
        transcripts_db,
        telemetry_handle,
        shutdown_notify,
        checkpoint_rx,
    );

    tokio::spawn(async move {
        worker.run().await;
    });

    TranscriptWorkerHandle { checkpoint_tx }
}

#[cfg(test)]
mod extract_event_timestamp_tests {
    use super::*;

    #[test]
    fn test_rfc3339() {
        let event = serde_json::json!({"timestamp": "2026-05-11T23:13:12.819Z"});
        assert_eq!(extract_event_timestamp(&event), Some(1778541192));
    }

    #[test]
    fn test_rfc3339_without_millis() {
        let event = serde_json::json!({"timestamp": "2026-05-12T00:21:05Z"});
        assert_eq!(extract_event_timestamp(&event), Some(1778545265));
    }

    #[test]
    fn test_rfc3339_subsecond_discarded() {
        let event = serde_json::json!({"timestamp": "2026-05-11T23:13:12.999Z"});
        assert_eq!(extract_event_timestamp(&event), Some(1778541192));
    }

    #[test]
    fn test_numeric_millis() {
        let event = serde_json::json!({"timestamp": 1759845073835u64});
        assert_eq!(extract_event_timestamp(&event), Some(1759845073));
    }

    #[test]
    fn test_missing_field() {
        let event = serde_json::json!({"type": "user.message"});
        assert_eq!(extract_event_timestamp(&event), None);
    }

    #[test]
    fn test_null_value() {
        let event = serde_json::json!({"timestamp": null});
        assert_eq!(extract_event_timestamp(&event), None);
    }

    #[test]
    fn test_invalid_string() {
        let event = serde_json::json!({"timestamp": "not-a-date"});
        assert_eq!(extract_event_timestamp(&event), None);
    }

    #[test]
    fn test_empty_string() {
        let event = serde_json::json!({"timestamp": ""});
        assert_eq!(extract_event_timestamp(&event), None);
    }
}

#[cfg(test)]
mod subagent_sweep_tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_worker(db: Arc<TranscriptsDatabase>) -> TranscriptWorker {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(Notify::new());
        let telemetry = DaemonTelemetryWorkerHandle::new_noop();
        TranscriptWorker::new(db, telemetry, shutdown, rx)
    }

    #[test]
    fn test_sweep_subagents_discovers_subagent_files() {
        let tmp = TempDir::new().unwrap();

        // Create a main session transcript: <project>/sess-abc.jsonl
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        let main_transcript = project_dir.join("sess-abc.jsonl");
        let mut f = std::fs::File::create(&main_transcript).unwrap();
        writeln!(f, r#"{{"type":"session","id":"sess-abc"}}"#).unwrap();

        // Create subagents directory: <project>/sess-abc/subagents/
        let subagents_dir = project_dir.join("sess-abc").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();

        // Create two subagent transcripts
        let sub1 = subagents_dir.join("agent-sub1.jsonl");
        let mut f = std::fs::File::create(&sub1).unwrap();
        writeln!(
            f,
            r#"{{"type":"message","message":{{"role":"user","content":"hi"}}}}"#
        )
        .unwrap();

        let sub2 = subagents_dir.join("agent-sub2.jsonl");
        let mut f = std::fs::File::create(&sub2).unwrap();
        writeln!(
            f,
            r#"{{"type":"message","message":{{"role":"assistant","content":"hello"}}}}"#
        )
        .unwrap();

        // Also create a .meta.json file that should be ignored
        let meta = subagents_dir.join("agent-sub1.meta.json");
        std::fs::File::create(&meta).unwrap();

        // Set up worker with DB
        let db_path = tmp.path().join("test.db");
        let db = Arc::new(TranscriptsDatabase::open(&db_path).unwrap());
        let mut worker = make_worker(db.clone());

        let notification = CheckpointNotification {
            session_id: "internal-sess-abc".to_string(),
            tool: "claude".to_string(),
            trace_id: "trace-1".to_string(),
            tool_use_id: None,
            transcript_path: main_transcript.clone(),
            repo_work_dir: Some(tmp.path().to_path_buf()),
        };

        worker.sweep_subagents_for_session(&notification);

        // Should have enqueued 2 subagent tasks
        assert_eq!(worker.priority_queue.len(), 2);

        // Both should be in the DB
        let sub1_sid = generate_session_id("agent-sub1", "claude");
        let sub2_sid = generate_session_id("agent-sub2", "claude");

        let rec1 = db.get_session(&sub1_sid).unwrap().unwrap();
        assert_eq!(rec1.external_session_id, "agent-sub1");
        assert_eq!(rec1.external_parent_session_id.as_deref(), Some("sess-abc"));
        assert_eq!(rec1.tool, "claude");

        let rec2 = db.get_session(&sub2_sid).unwrap().unwrap();
        assert_eq!(rec2.external_session_id, "agent-sub2");
        assert_eq!(rec2.external_parent_session_id.as_deref(), Some("sess-abc"));
    }

    #[test]
    fn test_sweep_subagents_no_dir_is_noop() {
        let tmp = TempDir::new().unwrap();
        let main_transcript = tmp.path().join("sess-xyz.jsonl");
        let mut f = std::fs::File::create(&main_transcript).unwrap();
        writeln!(f, r#"{{"type":"session"}}"#).unwrap();

        let db_path = tmp.path().join("test.db");
        let db = Arc::new(TranscriptsDatabase::open(&db_path).unwrap());
        let mut worker = make_worker(db.clone());

        let notification = CheckpointNotification {
            session_id: "internal-sess-xyz".to_string(),
            tool: "claude".to_string(),
            trace_id: "trace-2".to_string(),
            tool_use_id: None,
            transcript_path: main_transcript,
            repo_work_dir: None,
        };

        worker.sweep_subagents_for_session(&notification);
        assert_eq!(worker.priority_queue.len(), 0);
    }

    #[tokio::test]
    async fn test_handle_checkpoint_skips_subagent_sweep_for_non_claude() {
        let tmp = TempDir::new().unwrap();
        let main_transcript = tmp.path().join("sess-abc.jsonl");
        std::fs::File::create(&main_transcript).unwrap();

        let subagents_dir = tmp.path().join("sess-abc").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let sub = subagents_dir.join("agent-sub1.jsonl");
        let mut f = std::fs::File::create(&sub).unwrap();
        writeln!(f, r#"{{"type":"message"}}"#).unwrap();

        let db_path = tmp.path().join("test.db");
        let db = Arc::new(TranscriptsDatabase::open(&db_path).unwrap());
        let mut worker = make_worker(db.clone());

        let notification = CheckpointNotification {
            session_id: "internal-sess-abc".to_string(),
            tool: "copilot".to_string(),
            trace_id: "trace-3".to_string(),
            tool_use_id: None,
            transcript_path: main_transcript,
            repo_work_dir: None,
        };

        worker.handle_checkpoint_notification(notification).await;

        // Only the main session should be enqueued — no subagent sweep for copilot
        assert_eq!(worker.priority_queue.len(), 1);
        let task = worker.priority_queue.pop().unwrap();
        assert_eq!(task.session_id, "internal-sess-abc");
        assert_eq!(task.tool, "copilot");
    }

    #[test]
    fn test_sweep_subagents_deduplicates_in_flight() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();

        let main_transcript = project_dir.join("sess-dup.jsonl");
        std::fs::File::create(&main_transcript).unwrap();

        let subagents_dir = project_dir.join("sess-dup").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let sub = subagents_dir.join("agent-inflight.jsonl");
        let mut f = std::fs::File::create(&sub).unwrap();
        writeln!(f, r#"{{"type":"message"}}"#).unwrap();

        let db_path = tmp.path().join("test.db");
        let db = Arc::new(TranscriptsDatabase::open(&db_path).unwrap());
        let mut worker = make_worker(db.clone());

        // Mark the subagent's canonical path as in-flight
        let canonical = std::fs::canonicalize(&sub).unwrap();
        worker.in_flight.insert(canonical);

        let notification = CheckpointNotification {
            session_id: "internal-sess-dup".to_string(),
            tool: "claude".to_string(),
            trace_id: "trace-4".to_string(),
            tool_use_id: None,
            transcript_path: main_transcript,
            repo_work_dir: None,
        };

        worker.sweep_subagents_for_session(&notification);

        // Should not enqueue the in-flight subagent
        assert_eq!(worker.priority_queue.len(), 0);
    }
}
