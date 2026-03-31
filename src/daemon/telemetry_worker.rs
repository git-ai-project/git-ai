//! Daemon-side telemetry worker that batches and dispatches events.
//!
//! Runs inside the daemon process using tokio. Accumulates telemetry envelopes
//! and CAS payloads, then flushes them to their destinations every 3 seconds.

use crate::api::{ApiClient, ApiContext, CasObject, CasUploadRequest, upload_metrics_with_retry};
use crate::config::{Config, get_or_create_distinct_id};
use crate::daemon::control_api::{CasSyncPayload, TelemetryEnvelope};
use crate::metrics::db::MetricsDatabase;
use crate::metrics::{MetricEvent, MetricsBatch};
use crate::observability::MAX_METRICS_PER_ENVELOPE;
use crate::utils::debug_log;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, interval};

const FLUSH_INTERVAL: Duration = Duration::from_secs(3);

/// Accumulated telemetry events waiting to be flushed.
struct TelemetryBuffer {
    errors: Vec<ErrorEvent>,
    performances: Vec<PerformanceEvent>,
    messages: Vec<MessageEvent>,
    metrics: Vec<MetricEvent>,
    cas_records: Vec<CasSyncPayload>,
}

struct ErrorEvent {
    timestamp: String,
    message: String,
    context: Option<Value>,
}

struct PerformanceEvent {
    timestamp: String,
    operation: String,
    duration_ms: u128,
    context: Option<Value>,
    tags: Option<std::collections::HashMap<String, String>>,
}

struct MessageEvent {
    timestamp: String,
    message: String,
    level: String,
    context: Option<Value>,
}

impl TelemetryBuffer {
    fn new() -> Self {
        Self {
            errors: Vec::new(),
            performances: Vec::new(),
            messages: Vec::new(),
            metrics: Vec::new(),
            cas_records: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.errors.is_empty()
            && self.performances.is_empty()
            && self.messages.is_empty()
            && self.metrics.is_empty()
            && self.cas_records.is_empty()
    }

    fn ingest_envelopes(&mut self, envelopes: Vec<TelemetryEnvelope>) {
        for envelope in envelopes {
            match envelope {
                TelemetryEnvelope::Error {
                    timestamp,
                    message,
                    context,
                } => {
                    self.errors.push(ErrorEvent {
                        timestamp,
                        message,
                        context,
                    });
                }
                TelemetryEnvelope::Performance {
                    timestamp,
                    operation,
                    duration_ms,
                    context,
                    tags,
                } => {
                    self.performances.push(PerformanceEvent {
                        timestamp,
                        operation,
                        duration_ms,
                        context,
                        tags,
                    });
                }
                TelemetryEnvelope::Message {
                    timestamp,
                    message,
                    level,
                    context,
                } => {
                    self.messages.push(MessageEvent {
                        timestamp,
                        message,
                        level,
                        context,
                    });
                }
                TelemetryEnvelope::Metrics { events } => {
                    self.metrics.extend(events);
                }
            }
        }
    }

    fn ingest_cas(&mut self, records: Vec<CasSyncPayload>) {
        self.cas_records.extend(records);
    }

    fn take(&mut self) -> TelemetryBuffer {
        TelemetryBuffer {
            errors: std::mem::take(&mut self.errors),
            performances: std::mem::take(&mut self.performances),
            messages: std::mem::take(&mut self.messages),
            metrics: std::mem::take(&mut self.metrics),
            cas_records: std::mem::take(&mut self.cas_records),
        }
    }
}

/// Handle for submitting telemetry directly within the daemon process.
#[derive(Clone)]
pub struct DaemonTelemetryWorkerHandle {
    buffer: Arc<Mutex<TelemetryBuffer>>,
}

impl DaemonTelemetryWorkerHandle {
    /// Submit telemetry envelopes for batched processing.
    pub async fn submit_telemetry(&self, envelopes: Vec<TelemetryEnvelope>) {
        self.buffer.lock().await.ingest_envelopes(envelopes);
    }

    /// Submit CAS records for batched upload.
    pub async fn submit_cas(&self, records: Vec<CasSyncPayload>) {
        self.buffer.lock().await.ingest_cas(records);
    }

    /// Submit telemetry envelopes synchronously (best-effort, non-blocking).
    ///
    /// Used by the daemon process's own `observability::log_*()` calls which
    /// cannot go through the control socket (the daemon can't connect to itself).
    /// Uses `try_lock()` to avoid blocking the caller if the buffer is contested.
    pub fn submit_telemetry_sync(&self, envelopes: Vec<TelemetryEnvelope>) {
        if let Ok(mut buf) = self.buffer.try_lock() {
            buf.ingest_envelopes(envelopes);
        }
    }

    /// Submit CAS records synchronously (best-effort, non-blocking).
    ///
    /// Used by daemon-owned post-commit paths that cannot route through the
    /// control socket because the daemon cannot connect to itself.
    pub fn submit_cas_sync(&self, records: Vec<CasSyncPayload>) {
        if let Ok(mut buf) = self.buffer.try_lock() {
            buf.ingest_cas(records);
        }
    }
}

/// Global handle for the daemon's in-process telemetry worker.
///
/// Set once when the daemon spawns its telemetry worker, allowing
/// `observability::log_*()` functions to route events directly into
/// the worker buffer when running inside the daemon process.
static DAEMON_INTERNAL_TELEMETRY: std::sync::OnceLock<DaemonTelemetryWorkerHandle> =
    std::sync::OnceLock::new();

/// Register the daemon's in-process telemetry worker handle.
/// Called once during daemon startup after `spawn_telemetry_worker()`.
pub fn set_daemon_internal_telemetry(handle: DaemonTelemetryWorkerHandle) {
    let _ = DAEMON_INTERNAL_TELEMETRY.set(handle);
}

/// Submit telemetry from within the daemon process (sync, best-effort).
/// Returns true if the handle was available and envelopes were submitted.
pub fn submit_daemon_internal_telemetry(envelopes: Vec<TelemetryEnvelope>) -> bool {
    if let Some(handle) = DAEMON_INTERNAL_TELEMETRY.get() {
        handle.submit_telemetry_sync(envelopes);
        true
    } else {
        false
    }
}

/// Submit CAS records from within the daemon process (sync, best-effort).
/// Returns true if the handle was available and records were submitted.
pub fn submit_daemon_internal_cas(records: Vec<CasSyncPayload>) -> bool {
    if let Some(handle) = DAEMON_INTERNAL_TELEMETRY.get() {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let handle = handle.clone();
            runtime.spawn(async move {
                handle.submit_cas(records).await;
            });
        } else {
            handle.submit_cas_sync(records);
        }
        true
    } else {
        false
    }
}

/// Spawn the telemetry worker task. Returns a handle for submitting events.
///
/// The worker runs a flush loop every 3 seconds, sending accumulated events
/// to their respective destinations (Sentry, PostHog, metrics API, CAS API).
pub fn spawn_telemetry_worker() -> DaemonTelemetryWorkerHandle {
    let buffer = Arc::new(Mutex::new(TelemetryBuffer::new()));
    let handle = DaemonTelemetryWorkerHandle {
        buffer: buffer.clone(),
    };

    tokio::spawn(async move {
        telemetry_flush_loop(buffer).await;
    });

    handle
}

async fn telemetry_flush_loop(buffer: Arc<Mutex<TelemetryBuffer>>) {
    let mut ticker = interval(FLUSH_INTERVAL);
    // The first tick completes immediately; skip it.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        let snapshot = {
            let mut buf = buffer.lock().await;
            if buf.is_empty() {
                continue;
            }
            buf.take()
        };

        // Flush in a blocking task since the underlying HTTP clients are synchronous.
        tokio::task::spawn_blocking(move || {
            flush_telemetry_batch(snapshot);
        })
        .await
        .unwrap_or_else(|e| {
            debug_log(&format!("telemetry flush task panicked: {}", e));
        });
    }
}

fn flush_telemetry_batch(batch: TelemetryBuffer) {
    let config = Config::get();
    let distinct_id = get_or_create_distinct_id();

    // Flush metrics (always processed — uploaded or stored in SQLite)
    if !batch.metrics.is_empty() {
        flush_metrics(&batch.metrics);
    }

    // Flush Sentry events (errors, performance, messages)
    let has_sentry_or_posthog =
        !batch.errors.is_empty() || !batch.performances.is_empty() || !batch.messages.is_empty();

    if has_sentry_or_posthog {
        flush_sentry_and_posthog(
            config,
            &distinct_id,
            &batch.errors,
            &batch.performances,
            &batch.messages,
        );
    }

    // Flush CAS records
    if !batch.cas_records.is_empty() {
        flush_cas(batch.cas_records);
    }
}

fn flush_metrics(events: &[MetricEvent]) {
    let context = ApiContext::new(None);
    let api_base_url = context.base_url.clone();
    let client = ApiClient::new(context);

    let using_default_api = api_base_url == crate::config::DEFAULT_API_BASE_URL;
    let should_upload = !using_default_api || client.is_logged_in() || client.has_api_key();

    for chunk in events.chunks(MAX_METRICS_PER_ENVELOPE) {
        let batch = MetricsBatch::new(chunk.to_vec());
        if should_upload {
            match upload_metrics_with_retry(&client, &batch, "daemon_telemetry") {
                Ok(()) => continue,
                Err(_) => {
                    store_metrics_in_db(chunk);
                    continue;
                }
            }
        }
        store_metrics_in_db(chunk);
    }
}

fn store_metrics_in_db(events: &[MetricEvent]) {
    if events.is_empty() {
        return;
    }

    let event_jsons: Vec<String> = events
        .iter()
        .filter_map(|e| serde_json::to_string(e).ok())
        .collect();

    if event_jsons.is_empty() {
        return;
    }

    if let Ok(db) = MetricsDatabase::global()
        && let Ok(mut db_lock) = db.lock()
    {
        let _ = db_lock.insert_events(&event_jsons);
    }
}

fn flush_sentry_and_posthog(
    config: &Config,
    distinct_id: &str,
    errors: &[ErrorEvent],
    performances: &[PerformanceEvent],
    messages: &[MessageEvent],
) {
    // Check for Enterprise DSN
    let enterprise_dsn = config
        .telemetry_enterprise_dsn()
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::var("SENTRY_ENTERPRISE")
                .ok()
                .or_else(|| option_env!("SENTRY_ENTERPRISE").map(|s| s.to_string()))
                .filter(|s| !s.is_empty())
        });

    // Check for OSS DSN
    let oss_dsn = if config.is_telemetry_oss_disabled() {
        None
    } else {
        std::env::var("SENTRY_OSS")
            .ok()
            .or_else(|| option_env!("SENTRY_OSS").map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
    };

    // Check for PostHog configuration
    let posthog_api_key = if config.is_telemetry_oss_disabled() {
        None
    } else {
        std::env::var("POSTHOG_API_KEY")
            .ok()
            .or_else(|| option_env!("POSTHOG_API_KEY").map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
    };

    let posthog_host = std::env::var("POSTHOG_HOST")
        .ok()
        .or_else(|| option_env!("POSTHOG_HOST").map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://us.i.posthog.com".to_string());

    // Build Sentry clients
    let oss_client = oss_dsn.and_then(|dsn| SentryClient::from_dsn(&dsn));
    let enterprise_client = enterprise_dsn.and_then(|dsn| SentryClient::from_dsn(&dsn));

    // Build base tags
    let mut base_tags = BTreeMap::new();
    base_tags.insert("os".to_string(), json!(std::env::consts::OS));
    base_tags.insert("arch".to_string(), json!(std::env::consts::ARCH));
    base_tags.insert("distinct_id".to_string(), json!(distinct_id));

    // Send errors
    for error in errors {
        let mut extra = BTreeMap::new();
        if let Some(ctx) = &error.context
            && let Some(obj) = ctx.as_object()
        {
            for (key, value) in obj {
                extra.insert(key.clone(), value.clone());
            }
        }

        let event = json!({
            "message": error.message,
            "level": "error",
            "timestamp": error.timestamp,
            "platform": "other",
            "tags": base_tags,
            "extra": extra,
            "release": format!("git-ai@{}", env!("CARGO_PKG_VERSION")),
        });

        if let Some(client) = &oss_client {
            let _ = client.send_event(event.clone());
        }
        if let Some(client) = &enterprise_client {
            let _ = client.send_event(event);
        }
    }

    // Send performance events
    for perf in performances {
        let mut extra = BTreeMap::new();
        extra.insert("operation".to_string(), json!(perf.operation));
        extra.insert("duration_ms".to_string(), json!(perf.duration_ms));
        if let Some(ctx) = &perf.context
            && let Some(obj) = ctx.as_object()
        {
            for (key, value) in obj {
                extra.insert(key.clone(), value.clone());
            }
        }

        let mut perf_tags = base_tags.clone();
        if let Some(tags) = &perf.tags {
            for (key, value) in tags {
                perf_tags.insert(key.clone(), json!(value));
            }
        }

        let event = json!({
            "message": format!("Performance: {} ({}ms)", perf.operation, perf.duration_ms),
            "level": "info",
            "timestamp": perf.timestamp,
            "platform": "other",
            "tags": perf_tags,
            "extra": extra,
            "release": format!("git-ai@{}", env!("CARGO_PKG_VERSION")),
        });

        if let Some(client) = &oss_client {
            let _ = client.send_event(event.clone());
        }
        if let Some(client) = &enterprise_client {
            let _ = client.send_event(event);
        }
    }

    // Send messages (to Sentry + PostHog)
    for msg in messages {
        let mut extra = BTreeMap::new();
        if let Some(ctx) = &msg.context
            && let Some(obj) = ctx.as_object()
        {
            for (key, value) in obj {
                extra.insert(key.clone(), value.clone());
            }
        }

        let sentry_event = json!({
            "message": msg.message,
            "level": msg.level,
            "timestamp": msg.timestamp,
            "platform": "other",
            "tags": base_tags,
            "extra": extra,
            "release": format!("git-ai@{}", env!("CARGO_PKG_VERSION")),
        });

        if let Some(client) = &oss_client {
            let _ = client.send_event(sentry_event.clone());
        }
        if let Some(client) = &enterprise_client {
            let _ = client.send_event(sentry_event);
        }

        // PostHog only gets messages
        if let Some(api_key) = &posthog_api_key {
            let mut properties = BTreeMap::new();
            properties.insert("os".to_string(), json!(std::env::consts::OS));
            properties.insert("arch".to_string(), json!(std::env::consts::ARCH));
            properties.insert("version".to_string(), json!(env!("CARGO_PKG_VERSION")));
            properties.insert("message".to_string(), json!(msg.message));
            properties.insert("level".to_string(), json!(msg.level));
            if let Some(ctx) = &msg.context
                && let Some(obj) = ctx.as_object()
            {
                for (key, value) in obj {
                    properties.insert(key.clone(), value.clone());
                }
            }

            let endpoint = format!("{}/capture/", posthog_host.trim_end_matches('/'));
            let mut ph_event = json!({
                "api_key": api_key,
                "event": msg.message,
                "properties": properties,
                "distinct_id": distinct_id,
            });
            ph_event["timestamp"] = json!(msg.timestamp);

            let _ = minreq::post(&endpoint)
                .with_header("Content-Type", "application/json")
                .with_body(serde_json::to_string(&ph_event).unwrap_or_default())
                .send();
        }
    }
}

fn flush_cas(records: Vec<CasSyncPayload>) {
    let context = ApiContext::new(None);
    let api_base_url = context.base_url.clone();
    let client = ApiClient::new(context);

    let using_default_api = api_base_url == crate::config::DEFAULT_API_BASE_URL;
    if using_default_api && !client.is_logged_in() && !client.has_api_key() {
        debug_log("daemon telemetry: skipping CAS flush, not logged in");
        return;
    }

    // Build upload request
    let mut cas_objects = Vec::new();
    for record in &records {
        let content: Value = match serde_json::from_str(&record.data) {
            Ok(v) => v,
            Err(e) => {
                debug_log(&format!("daemon telemetry: CAS parse error: {}", e));
                continue;
            }
        };
        // Convert serialized JSON metadata string to HashMap
        let metadata = record
            .metadata
            .as_ref()
            .and_then(|m| serde_json::from_str::<std::collections::HashMap<String, String>>(m).ok())
            .unwrap_or_default();
        cas_objects.push(CasObject {
            content,
            hash: record.hash.clone(),
            metadata,
        });
    }

    if cas_objects.is_empty() {
        return;
    }

    for chunk in cas_objects.chunks(50) {
        let hashes: Vec<String> = chunk.iter().map(|o| o.hash.clone()).collect();
        let request = CasUploadRequest {
            objects: chunk.to_vec(),
        };
        match client.upload_cas(request) {
            Ok(_response) => {
                // Delete successfully uploaded records from the internal DB queue
                // so they don't accumulate as stale entries.
                if let Ok(db) = crate::authorship::internal_db::InternalDatabase::global()
                    && let Ok(mut db_lock) = db.lock()
                {
                    let _ = db_lock.delete_cas_by_hashes(&hashes);
                }
                debug_log(&format!(
                    "daemon telemetry: uploaded {} CAS objects",
                    chunk.len()
                ));
            }
            Err(e) => {
                debug_log(&format!("daemon telemetry: CAS upload error: {}", e));
            }
        }
    }
}

/// Minimal Sentry client (mirrors flush.rs SentryClient)
struct SentryClient {
    endpoint: String,
    public_key: String,
}

impl SentryClient {
    fn from_dsn(dsn: &str) -> Option<Self> {
        let url = url::Url::parse(dsn).ok()?;
        let public_key = url.username().to_string();
        let host = url.host_str()?;
        let project_id = url.path().trim_start_matches('/');
        let scheme = url.scheme();
        let endpoint = format!("{}://{}/api/{}/store/", scheme, host, project_id);
        Some(SentryClient {
            endpoint,
            public_key,
        })
    }

    fn send_event(&self, event: Value) -> Result<(), Box<dyn std::error::Error>> {
        let auth_header = format!(
            "Sentry sentry_version=7, sentry_key={}, sentry_client=git-ai/{}",
            self.public_key,
            env!("CARGO_PKG_VERSION")
        );

        let body = serde_json::to_string(&event)?;
        let response = minreq::post(&self.endpoint)
            .with_header("X-Sentry-Auth", auth_header)
            .with_header("Content-Type", "application/json")
            .with_body(body)
            .send()?;

        let status = response.status_code;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(format!("Sentry returned status {}", status).into())
        }
    }
}
