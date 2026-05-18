use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::api_client::ApiClient;
use super::telemetry_queue::TelemetryQueue;
use super::telemetry_types::{
    CasObject, CasUploadRequest, MAX_CAS_OBJECTS_PER_UPLOAD, MetricEvent, MetricsBatch,
};

const FLUSH_INTERVAL: Duration = Duration::from_secs(3);
const MAX_METRICS_PER_BATCH: usize = 1000;
const QUEUE_DRAIN_BATCH: usize = 20;

/// Thread-safe telemetry buffer.
struct TelemetryBuffer {
    metrics: Vec<MetricEvent>,
    cas_objects: Vec<CasObject>,
}

impl TelemetryBuffer {
    fn new() -> Self {
        Self {
            metrics: Vec::new(),
            cas_objects: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.metrics.is_empty() && self.cas_objects.is_empty()
    }

    fn take(&mut self) -> TelemetryBuffer {
        TelemetryBuffer {
            metrics: std::mem::take(&mut self.metrics),
            cas_objects: std::mem::take(&mut self.cas_objects),
        }
    }
}

/// Handle for submitting telemetry events from other threads.
#[derive(Clone)]
pub struct TelemetryHandle {
    buffer: Arc<Mutex<TelemetryBuffer>>,
}

impl TelemetryHandle {
    /// Submit metric events for batched upload.
    pub fn submit_metrics(&self, events: Vec<MetricEvent>) {
        if let Ok(mut buf) = self.buffer.lock() {
            buf.metrics.extend(events);
        }
    }

    /// Submit a single metric event.
    pub fn submit_metric(&self, event: MetricEvent) {
        if let Ok(mut buf) = self.buffer.lock() {
            buf.metrics.push(event);
        }
    }

    /// Submit a CAS object for upload.
    /// The hash is computed as SHA256 of the JSON-serialized content.
    /// Objects larger than 512KB are dropped as a safety bound against
    /// accidentally transmitting source code or large artifacts.
    pub fn submit_cas(
        &self,
        content: serde_json::Value,
        metadata: std::collections::HashMap<String, String>,
    ) {
        let content_json = serde_json::to_string(&content).unwrap_or_default();

        const MAX_CAS_PAYLOAD_BYTES: usize = 512 * 1024;
        if content_json.len() > MAX_CAS_PAYLOAD_BYTES {
            eprintln!(
                "[git-ai daemon] dropping oversized CAS object ({} bytes, limit {})",
                content_json.len(),
                MAX_CAS_PAYLOAD_BYTES
            );
            return;
        }

        let hash = {
            let mut hasher = Sha256::new();
            hasher.update(content_json.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let object = CasObject {
            content,
            hash,
            metadata,
        };

        if let Ok(mut buf) = self.buffer.lock() {
            buf.cas_objects.push(object);
        }
    }
}

/// Spawn the telemetry worker thread. Returns a handle for submitting events.
///
/// The worker flushes accumulated events every 3 seconds, uploading metrics
/// and CAS objects to the backend. Failed uploads are persisted to a SQLite
/// queue for retry on subsequent flush cycles. Runs until `shutdown` is set.
pub fn spawn_telemetry_worker(shutdown: Arc<AtomicBool>) -> TelemetryHandle {
    let buffer = Arc::new(Mutex::new(TelemetryBuffer::new()));
    let handle = TelemetryHandle {
        buffer: buffer.clone(),
    };

    thread::Builder::new()
        .name("telemetry-worker".to_string())
        .spawn(move || {
            telemetry_flush_loop(buffer, shutdown);
        })
        .expect("failed to spawn telemetry worker thread");

    handle
}

fn queue_db_path() -> PathBuf {
    crate::paths::git_ai_internal_dir().join("telemetry_queue.db")
}

fn telemetry_flush_loop(buffer: Arc<Mutex<TelemetryBuffer>>, shutdown: Arc<AtomicBool>) {
    eprintln!("[git-ai daemon] telemetry worker started");

    loop {
        thread::sleep(FLUSH_INTERVAL);

        if shutdown.load(Ordering::Relaxed) {
            // Final flush before exit
            if let Ok(mut buf) = buffer.lock()
                && !buf.is_empty()
            {
                let snapshot = buf.take();
                drop(buf);
                flush_batch(snapshot);
            }
            break;
        }

        let snapshot = {
            let mut buf = match buffer.lock() {
                Ok(b) => b,
                Err(_) => continue,
            };
            if buf.is_empty() {
                continue;
            }
            buf.take()
        };

        flush_batch(snapshot);
    }

    eprintln!("[git-ai daemon] telemetry worker stopped");
}

fn flush_batch(batch: TelemetryBuffer) {
    let client = ApiClient::new();
    let queue = TelemetryQueue::open(&queue_db_path()).ok();

    if !batch.metrics.is_empty() {
        flush_metrics(&client, batch.metrics, queue.as_ref());
    }

    if !batch.cas_objects.is_empty() {
        flush_cas(&client, batch.cas_objects, queue.as_ref());
    }

    if let Some(ref q) = queue {
        drain_queued_metrics(&client, q);
        drain_queued_cas(&client, q);
    }
}

fn flush_metrics(client: &ApiClient, events: Vec<MetricEvent>, queue: Option<&TelemetryQueue>) {
    if !client.should_upload() {
        if let Some(q) = queue
            && let Err(e) = q.enqueue_metrics(&events)
        {
            eprintln!("[git-ai daemon] failed to queue metrics offline: {}", e);
        }
        return;
    }

    for chunk in events.chunks(MAX_METRICS_PER_BATCH) {
        let batch = MetricsBatch::new(chunk.to_vec());
        if let Err(e) = client.upload_metrics_with_retry(&batch) {
            eprintln!(
                "[git-ai daemon] metrics upload failed, queuing offline: {}",
                e
            );
            if let Some(q) = queue
                && let Err(qe) = q.enqueue_metrics(chunk)
            {
                eprintln!("[git-ai daemon] failed to queue metrics offline: {}", qe);
            }
        }
    }
}

fn flush_cas(client: &ApiClient, objects: Vec<CasObject>, queue: Option<&TelemetryQueue>) {
    if !client.should_upload() {
        if let Some(q) = queue
            && let Err(e) = q.enqueue_cas(&objects)
        {
            eprintln!("[git-ai daemon] failed to queue CAS offline: {}", e);
        }
        return;
    }

    for chunk in objects.chunks(MAX_CAS_OBJECTS_PER_UPLOAD) {
        let request = CasUploadRequest {
            objects: chunk.to_vec(),
        };
        match client.upload_cas(&request) {
            Ok(response) => {
                if response.failure_count > 0 {
                    eprintln!(
                        "[git-ai daemon] CAS upload: {} succeeded, {} failed",
                        response.success_count, response.failure_count
                    );
                }
            }
            Err(e) => {
                eprintln!("[git-ai daemon] CAS upload failed, queuing offline: {}", e);
                if let Some(q) = queue
                    && let Err(qe) = q.enqueue_cas(chunk)
                {
                    eprintln!("[git-ai daemon] failed to queue CAS offline: {}", qe);
                }
            }
        }
    }
}

fn drain_queued_metrics(client: &ApiClient, queue: &TelemetryQueue) {
    if !client.should_upload() {
        return;
    }

    let batches = match queue.drain_metrics(QUEUE_DRAIN_BATCH) {
        Ok(b) => b,
        Err(_) => return,
    };

    let mut uploaded_ids = Vec::new();
    for (id, events) in &batches {
        for chunk in events.chunks(MAX_METRICS_PER_BATCH) {
            let batch = MetricsBatch::new(chunk.to_vec());
            if client.upload_metrics(&batch).is_err() {
                if !uploaded_ids.is_empty() {
                    let _ = queue.delete_metrics(&uploaded_ids);
                }
                return;
            }
        }
        uploaded_ids.push(*id);
    }

    if !uploaded_ids.is_empty() {
        let _ = queue.delete_metrics(&uploaded_ids);
    }
}

fn drain_queued_cas(client: &ApiClient, queue: &TelemetryQueue) {
    if !client.should_upload() {
        return;
    }

    let batches = match queue.drain_cas(QUEUE_DRAIN_BATCH) {
        Ok(b) => b,
        Err(_) => return,
    };

    let mut uploaded_ids = Vec::new();
    for (id, objects) in &batches {
        for chunk in objects.chunks(MAX_CAS_OBJECTS_PER_UPLOAD) {
            let request = CasUploadRequest {
                objects: chunk.to_vec(),
            };
            if client.upload_cas(&request).is_err() {
                if !uploaded_ids.is_empty() {
                    let _ = queue.delete_cas(&uploaded_ids);
                }
                return;
            }
        }
        uploaded_ids.push(*id);
    }

    if !uploaded_ids.is_empty() {
        let _ = queue.delete_cas(&uploaded_ids);
    }
}

/// Flush all pending items from the SQLite queue. Used by the `flush-metrics-db` command.
pub fn flush_queue_now() -> Result<(usize, usize), String> {
    let queue = TelemetryQueue::open(&queue_db_path())?;
    let client = ApiClient::new();

    if !client.should_upload() {
        let mc = queue.pending_metrics_count()?;
        let cc = queue.pending_cas_count()?;
        return Err(format!(
            "cannot upload: not authenticated. {} metrics and {} CAS objects remain queued.",
            mc, cc
        ));
    }

    let mut metrics_flushed = 0usize;
    loop {
        let batches = queue.drain_metrics(QUEUE_DRAIN_BATCH)?;
        if batches.is_empty() {
            break;
        }
        let mut ids = Vec::new();
        for (id, events) in &batches {
            for chunk in events.chunks(MAX_METRICS_PER_BATCH) {
                let batch = MetricsBatch::new(chunk.to_vec());
                client.upload_metrics_with_retry(&batch)?;
            }
            metrics_flushed += events.len();
            ids.push(*id);
        }
        queue.delete_metrics(&ids)?;
    }

    let mut cas_flushed = 0usize;
    loop {
        let batches = queue.drain_cas(QUEUE_DRAIN_BATCH)?;
        if batches.is_empty() {
            break;
        }
        let mut ids = Vec::new();
        for (id, objects) in &batches {
            for chunk in objects.chunks(MAX_CAS_OBJECTS_PER_UPLOAD) {
                let request = CasUploadRequest {
                    objects: chunk.to_vec(),
                };
                client
                    .upload_cas(&request)
                    .map_err(|e| format!("CAS upload: {}", e))?;
            }
            cas_flushed += objects.len();
            ids.push(*id);
        }
        queue.delete_cas(&ids)?;
    }

    Ok((metrics_flushed, cas_flushed))
}

/// Return the number of pending items in the offline queue.
pub fn queue_stats() -> Result<(usize, usize), String> {
    let queue = TelemetryQueue::open(&queue_db_path())?;
    let mc = queue.pending_metrics_count()?;
    let cc = queue.pending_cas_count()?;
    Ok((mc, cc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::telemetry_types::{MetricEventId, SparseArray};

    #[test]
    fn handle_submit_and_buffer() {
        let buffer = Arc::new(Mutex::new(TelemetryBuffer::new()));
        let handle = TelemetryHandle {
            buffer: buffer.clone(),
        };

        let event = MetricEvent::new(
            MetricEventId::Committed,
            SparseArray::new(),
            SparseArray::new(),
        );
        handle.submit_metric(event);

        let buf = buffer.lock().unwrap();
        assert_eq!(buf.metrics.len(), 1);
        assert_eq!(buf.metrics[0].event_id, 1);
    }

    #[test]
    fn cas_hash_is_sha256_of_content() {
        let buffer = Arc::new(Mutex::new(TelemetryBuffer::new()));
        let handle = TelemetryHandle {
            buffer: buffer.clone(),
        };

        let content = serde_json::json!({"test": "data"});
        handle.submit_cas(content.clone(), std::collections::HashMap::new());

        let buf = buffer.lock().unwrap();
        assert_eq!(buf.cas_objects.len(), 1);

        // Verify hash matches SHA256 of JSON content
        let expected_json = serde_json::to_string(&content).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(expected_json.as_bytes());
        let expected_hash = format!("{:x}", hasher.finalize());

        assert_eq!(buf.cas_objects[0].hash, expected_hash);
    }

    #[test]
    fn buffer_take_drains() {
        let mut buf = TelemetryBuffer::new();
        buf.metrics.push(MetricEvent::new(
            MetricEventId::Checkpoint,
            SparseArray::new(),
            SparseArray::new(),
        ));
        assert!(!buf.is_empty());

        let snapshot = buf.take();
        assert!(buf.is_empty());
        assert_eq!(snapshot.metrics.len(), 1);
    }
}
