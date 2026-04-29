//! Daemon-side transcript worker for polling and processing transcript changes.
//!
//! Runs inside the daemon process with three event sources:
//! 1. **Checkpoint notifications** (Immediate priority, <100ms) - fired when `git-ai checkpoint` is called
//! 2. **Polling detection** (High priority, <1s) - periodic file stat checks find modified transcripts
//! 3. **Historical backfill** (Low priority) - process old transcripts we haven't seen before

use crate::daemon::telemetry_worker::DaemonTelemetryWorkerHandle;
use crate::metrics::{EventAttributes, record};
use crate::transcripts::db::{SessionRecord, TranscriptsDatabase};
use crate::transcripts::processor::{TranscriptFormat, process_transcript};
use crate::transcripts::types::TranscriptError;
use crate::transcripts::watermark::WatermarkType;
use chrono::{TimeZone, Utc};
use std::collections::{BinaryHeap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::{Duration, interval};

const PROCESSING_TICK_INTERVAL: Duration = Duration::from_millis(100);
const POLLING_TICK_INTERVAL: Duration = Duration::from_secs(1);

/// Priority levels for processing tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    Low = 2,       // Historical backfill
    High = 1,      // Polling-detected modification
    Immediate = 0, // Checkpoint-triggered, process first
}

/// Task to process a session's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessingTask {
    priority: Priority,
    session_id: String,
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
        trace_id: String,
        transcript_path: PathBuf,
    ) {
        let notification = CheckpointNotification {
            session_id,
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
    #[allow(dead_code)] // May be used in future for enhanced telemetry
    trace_id: String,
    transcript_path: PathBuf,
}

/// Worker that processes transcript changes.
struct TranscriptWorker {
    transcripts_db: Arc<TranscriptsDatabase>,
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
        Self {
            transcripts_db,
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

        // Migrate internal.db if it exists
        if let Err(e) = self.migrate_internal_db().await {
            tracing::error!(error = %e, "failed to migrate internal.db");
        }

        // Discover existing sessions and queue for historical processing
        if let Err(e) = self.discover_sessions().await {
            tracing::error!(error = %e, "failed to discover sessions");
        }

        let mut processing_ticker = interval(PROCESSING_TICK_INTERVAL);
        let mut polling_ticker = interval(POLLING_TICK_INTERVAL);
        // Skip the first immediate tick
        processing_ticker.tick().await;
        polling_ticker.tick().await;

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
                _ = polling_ticker.tick() => {
                    if let Err(e) = self.detect_transcript_modifications().await {
                        tracing::error!(error = %e, "failed to detect transcript modifications");
                    }
                }
                Some(notification) = self.checkpoint_rx.recv() => {
                    self.handle_checkpoint_notification(notification).await;
                }
            }
        }

        tracing::info!("transcript worker shutdown complete");
    }

    /// Migrate old internal.db prompt records to sessions.
    async fn migrate_internal_db(&self) -> Result<(), String> {
        let Some(internal_dir) = crate::config::internal_dir_path() else {
            return Ok(());
        };
        let internal_db_path = internal_dir.join("internal.db");

        if !internal_db_path.exists() {
            return Ok(());
        }

        tracing::info!(path = %internal_db_path.display(), "migrating internal.db");

        // Open the old internal.db (read-only)
        let internal_conn = rusqlite::Connection::open_with_flags(
            &internal_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("failed to open internal.db: {}", e))?;

        // Read prompt records with transcript paths
        let mut stmt = internal_conn
            .prepare("SELECT id, messages_url FROM prompts WHERE messages_url IS NOT NULL")
            .map_err(|e| format!("failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let messages_url: String = row.get(1)?;
                Ok((id, messages_url))
            })
            .map_err(|e| format!("failed to query prompts: {}", e))?;

        let mut migrated = 0;
        for row in rows {
            let (id, messages_url) = row.map_err(|e| format!("failed to read row: {}", e))?;

            // Extract transcript path from messages_url (format: "file:///path/to/transcript.jsonl")
            if let Some(path_str) = messages_url.strip_prefix("file://") {
                let transcript_path = PathBuf::from(path_str);

                // Check if session already exists
                if self
                    .transcripts_db
                    .get_session(&id)
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    continue;
                }

                // Create session record
                let now = Utc::now().timestamp();
                let record = SessionRecord {
                    session_id: id.clone(),
                    agent_type: "unknown".to_string(),
                    transcript_path: transcript_path.display().to_string(),
                    transcript_format: "ClaudeJsonl".to_string(), // Assume Claude format
                    watermark_type: "ByteOffset".to_string(),
                    watermark_value: "0".to_string(),
                    model: None,
                    tool: None,
                    external_thread_id: None,
                    first_seen_at: now,
                    last_processed_at: 0, // Never processed
                    last_known_size: 0,
                    last_modified: None,
                    processing_errors: 0,
                    last_error: None,
                };

                if let Err(e) = self.transcripts_db.insert_session(&record) {
                    tracing::warn!(session_id = %id, error = %e, "failed to migrate session");
                } else {
                    migrated += 1;
                }
            }
        }

        drop(stmt);
        drop(internal_conn);

        // Rename internal.db to internal.db.deprecated
        let deprecated_path = internal_dir.join("internal.db.deprecated");
        std::fs::rename(&internal_db_path, &deprecated_path)
            .map_err(|e| format!("failed to rename internal.db: {}", e))?;

        tracing::info!(migrated, "internal.db migration complete");
        Ok(())
    }

    /// Discover sessions from transcript directories.
    async fn discover_sessions(&mut self) -> Result<(), String> {
        // For now, just queue all known sessions at Low priority
        let sessions = self
            .transcripts_db
            .all_sessions()
            .map_err(|e| format!("failed to list sessions: {}", e))?;

        for session in sessions {
            let canonical_path = std::fs::canonicalize(&session.transcript_path)
                .unwrap_or_else(|_| PathBuf::from(&session.transcript_path));

            self.priority_queue.push(ProcessingTask {
                priority: Priority::Low,
                session_id: session.session_id,
                canonical_path,
                retry_count: 0,
            });
        }

        tracing::info!(sessions = self.priority_queue.len(), "discovered sessions");
        Ok(())
    }

    /// Detect transcript modifications by comparing file metadata.
    async fn detect_transcript_modifications(&mut self) -> Result<(), String> {
        let sessions = self
            .transcripts_db
            .all_sessions()
            .map_err(|e| format!("failed to list sessions: {}", e))?;

        for session in sessions {
            let path = PathBuf::from(&session.transcript_path);
            if !path.exists() {
                continue;
            }

            let metadata = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let file_size = metadata.len() as i64;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);

            // Check if file has changed
            let has_changed = file_size != session.last_known_size
                || (modified.is_some() && modified != session.last_modified);

            if has_changed {
                let canonical_path = std::fs::canonicalize(&path).unwrap_or(path);

                // Deduplicate via in_flight
                if self.in_flight.contains(&canonical_path) {
                    continue;
                }

                self.priority_queue.push(ProcessingTask {
                    priority: Priority::High,
                    session_id: session.session_id.clone(),
                    canonical_path,
                    retry_count: 0,
                });
            }
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

        // Parse format
        let format = match session.transcript_format.as_str() {
            "ClaudeJsonl" => TranscriptFormat::ClaudeJsonl,
            "CursorJsonl" => TranscriptFormat::CursorJsonl,
            "DroidJsonl" => TranscriptFormat::DroidJsonl,
            "CopilotSessionJson" => TranscriptFormat::CopilotSessionJson,
            "CopilotEventStreamJsonl" => TranscriptFormat::CopilotEventStreamJsonl,
            _ => {
                return Err(TranscriptError::Parse {
                    line: 0,
                    message: format!("unknown transcript format: {}", session.transcript_format),
                });
            }
        };

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

        // Process transcript
        let batch = process_transcript(
            format,
            &PathBuf::from(&session.transcript_path),
            watermark,
            &session.session_id,
        )?;

        // Get event count before moving events
        let event_count = batch.events.len();

        // Emit events via metrics::record
        for event_values in batch.events {
            let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
                .session_id(session.session_id.clone());
            // Note: trace_id is embedded in the transcript events already
            record(event_values, attrs);
        }

        // Update watermark and metadata
        db.update_watermark(&session.session_id, batch.new_watermark.as_ref())?;

        // Update file metadata
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
