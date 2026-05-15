use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::api_client::ApiClient;
use super::telemetry_types::{
    CasObject, CasUploadRequest, MAX_CAS_OBJECTS_PER_UPLOAD, MetricEvent, MetricsBatch,
};

const FLUSH_INTERVAL: Duration = Duration::from_secs(3);
const MAX_METRICS_PER_BATCH: usize = 1000;

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
    pub fn submit_cas(
        &self,
        content: serde_json::Value,
        metadata: std::collections::HashMap<String, String>,
    ) {
        let content_json = serde_json::to_string(&content).unwrap_or_default();
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
/// and CAS objects to the backend. Runs until `shutdown` is set.
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

    if !batch.metrics.is_empty() {
        flush_metrics(&client, batch.metrics);
    }

    if !batch.cas_objects.is_empty() {
        flush_cas(&client, batch.cas_objects);
    }
}

fn flush_metrics(client: &ApiClient, events: Vec<MetricEvent>) {
    if !client.should_upload() {
        return;
    }

    for chunk in events.chunks(MAX_METRICS_PER_BATCH) {
        let batch = MetricsBatch::new(chunk.to_vec());
        if let Err(e) = client.upload_metrics_with_retry(&batch) {
            eprintln!("[git-ai daemon] metrics upload failed after retry: {}", e);
        }
    }
}

fn flush_cas(client: &ApiClient, objects: Vec<CasObject>) {
    if !client.should_upload() {
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
                eprintln!("[git-ai daemon] CAS upload failed: {}", e);
            }
        }
    }
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
