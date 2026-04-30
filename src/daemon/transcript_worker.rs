//! Daemon-side transcript worker for sweep-based transcript discovery.
//!
//! Runs inside the daemon process with two event sources:
//! 1. **Checkpoint notifications** (Immediate priority, <100ms) - fired when `git-ai checkpoint` is called
//! 2. **Periodic sweeps** (Low priority, every 30min) - agent-specific discovery of all sessions

use crate::daemon::telemetry_worker::DaemonTelemetryWorkerHandle;
use crate::metrics::{EventAttributes, record};
use crate::transcripts::db::TranscriptsDatabase;
use crate::transcripts::types::TranscriptError;
use crate::transcripts::watermark::WatermarkType;
use chrono::{TimeZone, Utc};
use std::collections::{BinaryHeap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::{Duration, interval};

const PROCESSING_TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Priority levels for processing tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    Low = 2, // Sweep-discovered sessions
    Immediate = 0, // Checkpoint-triggered, process first
             // REMOVED: High = 1 (was polling)
}

/// Task to process a session's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessingTask {
    priority: Priority,
    session_id: String,
    agent_type: String, // NEW: needed to get the right Agent impl
    canonical_path: PathBuf,
    retry_count: u32,
}

impl PartialOrd for ProcessingTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProcessingTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher priority first (Immediate=0 < High=1 < Low=2)
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.session_id.cmp(&other.session_id))
    }
}

/// Handle for sending checkpoint notifications to the worker.
#[derive(Clone)]
pub struct TranscriptWorkerHandle {
    checkpoint_tx: Arc<AsyncMutex<tokio::sync::mpsc::UnboundedSender<CheckpointNotification>>>,
}

impl TranscriptWorkerHandle {
    /// Notify the worker that a checkpoint was recorded.
    pub async fn notify_checkpoint(
        &self,
        session_id: String,
        agent_type: String, // NEW parameter
        trace_id: String,
        transcript_path: PathBuf,
    ) {
        let notification = CheckpointNotification {
            session_id,
            agent_type, // NEW field
            trace_id,
            transcript_path,
        };
        let tx = self.checkpoint_tx.lock().await;
        let _ = tx.send(notification);
    }
}

#[derive(Debug, Clone)]
struct CheckpointNotification {
    session_id: String,
    agent_type: String, // NEW: extracted from CheckpointRequest
    #[allow(dead_code)] // May be used in future for enhanced telemetry
    trace_id: String,
    transcript_path: PathBuf,
}

/// Worker that processes transcript changes.
struct TranscriptWorker {
    transcripts_db: Arc<TranscriptsDatabase>,
    sweep_coordinator: crate::daemon::sweep_coordinator::SweepCoordinator, // NEW
    priority_queue: BinaryHeap<ProcessingTask>,
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
            in_flight: HashSet::new(),
            telemetry_handle,
            shutdown_notify,
            checkpoint_rx,
        }
    }

    /// Main processing loop.
    async fn run(mut self) {
        tracing::info!("transcript worker started");

        let mut processing_ticker = interval(PROCESSING_TICK_INTERVAL);
        let mut sweep_ticker = interval(Duration::from_secs(30 * 60)); // NEW: 30 minutes

        // Skip the first immediate tick
        processing_ticker.tick().await;
        sweep_ticker.tick().await;

        // Run initial sweep on startup
        if let Err(e) = self.run_sweep().await {
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
                    if let Err(e) = self.run_sweep().await {
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
                agent_type: session.agent_type,
                canonical_path: session.canonical_path,
                retry_count: 0,
            });
        }

        Ok(())
    }

    /// Handle a checkpoint notification.
    async fn handle_checkpoint_notification(&mut self, notification: CheckpointNotification) {
        let canonical_path = std::fs::canonicalize(&notification.transcript_path)
            .unwrap_or_else(|_| notification.transcript_path.clone());

        // Deduplicate via in_flight
        if self.in_flight.contains(&canonical_path) {
            return;
        }

        self.priority_queue.push(ProcessingTask {
            priority: Priority::Immediate,
            session_id: notification.session_id,
            agent_type: notification.agent_type, // NEW: pass through agent_type
            canonical_path,
            retry_count: 0,
        });
    }

    /// Process the next task from the queue.
    async fn process_next_task(&mut self) {
        let Some(task) = self.priority_queue.pop() else {
            return;
        };

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
    fn process_session_blocking(
        db: &TranscriptsDatabase,
        _telemetry: &DaemonTelemetryWorkerHandle,
        task: &ProcessingTask,
    ) -> Result<(), TranscriptError> {
        let session = db
            .get_session(&task.session_id)?
            .ok_or_else(|| TranscriptError::Fatal {
                message: format!("session not found: {}", task.session_id),
            })?;

        // Get the agent implementation
        let agent = crate::transcripts::agent::get_agent(&task.agent_type).ok_or_else(|| {
            TranscriptError::Fatal {
                message: format!("unknown agent type: {}", task.agent_type),
            }
        })?;

        // Parse watermark type
        let watermark_type = match session.watermark_type.as_str() {
            "ByteOffset" => WatermarkType::ByteOffset,
            "Hybrid" => WatermarkType::Hybrid,
            _ => {
                return Err(TranscriptError::Parse {
                    line: 0,
                    message: format!("unknown watermark type: {}", session.watermark_type),
                });
            }
        };

        // Deserialize watermark
        let watermark = watermark_type.deserialize(&session.watermark_value)?;

        // Read transcript using agent
        let batch = agent.read_incremental(
            &PathBuf::from(&session.transcript_path),
            watermark,
            &session.session_id,
        )?;

        let event_count = batch.events.len();

        // Emit events via metrics::record
        for event_values in batch.events {
            let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
                .session_id(session.session_id.clone());
            record(event_values, attrs);
        }

        // Update watermark and metadata
        db.update_watermark(&session.session_id, batch.new_watermark.as_ref())?;

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
            events = event_count,
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
                    let _ = self
                        .transcripts_db
                        .record_error(&task.session_id, &format!("max retries: {}", message));
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

                // Re-queue with updated retry count
                let mut retried_task = task.clone();
                retried_task.retry_count = retry_count;
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
                let _ = self.transcripts_db.record_error(
                    &task.session_id,
                    &format!("parse line {}: {}", line, message),
                );
            }
            TranscriptError::Fatal { message } => {
                // Fatal errors are not retried
                tracing::error!(
                    session_id = %task.session_id,
                    error = %message,
                    "fatal error, skipping session"
                );
                let _ = self
                    .transcripts_db
                    .record_error(&task.session_id, &format!("fatal: {}", message));
            }
        }
    }

    /// Drain immediate priority tasks before shutdown.
    async fn drain_immediate_tasks(&mut self) {
        let mut immediate_tasks = Vec::new();

        // Collect all immediate tasks
        while let Some(task) = self.priority_queue.pop() {
            if task.priority == Priority::Immediate {
                immediate_tasks.push(task);
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

            if let Err(e) = result {
                tracing::error!(error = %e, session_id = %task.session_id, "failed to drain task");
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

    TranscriptWorkerHandle {
        checkpoint_tx: Arc::new(AsyncMutex::new(checkpoint_tx)),
    }
}
