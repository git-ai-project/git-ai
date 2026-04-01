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

use sentry::protocol::{Event as SentryEvent, Level as SentryLevel};
use sentry::types::Dsn;
use std::time::SystemTime;

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

    // Build long-lived Sentry clients once, reused across all flush cycles.
    // This avoids repeatedly creating/tearing down transport worker threads.
    let sentry_clients = Arc::new(SentryClients::new(Config::get()));

    loop {
        ticker.tick().await;

        let snapshot = {
            let mut buf = buffer.lock().await;
            if buf.is_empty() {
                continue;
            }
            buf.take()
        };

        let clients = sentry_clients.clone();
        // Flush in a blocking task since the underlying HTTP clients are synchronous.
        tokio::task::spawn_blocking(move || {
            flush_telemetry_batch(snapshot, &clients);
        })
        .await
        .unwrap_or_else(|e| {
            debug_log(&format!("telemetry flush task panicked: {}", e));
        });
    }
}

fn flush_telemetry_batch(batch: TelemetryBuffer, sentry_clients: &SentryClients) {
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
            sentry_clients,
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

/// Resolve the enterprise Sentry DSN from config, env var, or compile-time env.
fn resolve_enterprise_dsn(config: &Config) -> Option<String> {
    config
        .telemetry_enterprise_dsn()
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::var("SENTRY_ENTERPRISE")
                .ok()
                .or_else(|| option_env!("SENTRY_ENTERPRISE").map(|s| s.to_string()))
                .filter(|s| !s.is_empty())
        })
}

/// Resolve the OSS Sentry DSN from env var or compile-time env.
fn resolve_oss_dsn(config: &Config) -> Option<String> {
    if config.is_telemetry_oss_disabled() {
        None
    } else {
        std::env::var("SENTRY_OSS")
            .ok()
            .or_else(|| option_env!("SENTRY_OSS").map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
    }
}

/// Build a `sentry::Client` from a DSN string with standard options.
fn build_sentry_client(dsn: &str) -> Option<Arc<sentry::Client>> {
    let parsed_dsn: Dsn = dsn.parse().ok()?;
    let client = sentry::Client::with_options(sentry::ClientOptions {
        dsn: Some(parsed_dsn),
        release: Some(format!("git-ai@{}", env!("CARGO_PKG_VERSION")).into()),
        default_integrations: false,
        ..Default::default()
    });
    Some(Arc::new(client))
}

/// Build base tags applied to all Sentry events.
fn base_sentry_tags(distinct_id: &str) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    tags.insert("os".to_string(), std::env::consts::OS.to_string());
    tags.insert("arch".to_string(), std::env::consts::ARCH.to_string());
    tags.insert("distinct_id".to_string(), distinct_id.to_string());
    tags
}

/// Convert a JSON Value context map into a BTreeMap<String, Value> for Sentry extra.
fn context_to_extra(context: &Option<Value>) -> BTreeMap<String, Value> {
    let mut extra = BTreeMap::new();
    if let Some(ctx) = context
        && let Some(obj) = ctx.as_object()
    {
        for (key, value) in obj {
            extra.insert(key.clone(), value.clone());
        }
    }
    extra
}

/// Convert a level string (e.g. "error", "info", "warning") to a sentry Level.
fn parse_sentry_level(level: &str) -> SentryLevel {
    match level {
        "fatal" => SentryLevel::Fatal,
        "error" => SentryLevel::Error,
        "warning" => SentryLevel::Warning,
        "info" => SentryLevel::Info,
        _ => SentryLevel::Debug,
    }
}

/// Parse an RFC 3339 timestamp string into a `SystemTime`.
/// Falls back to `SystemTime::now()` if parsing fails.
fn parse_timestamp(ts: &str) -> SystemTime {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .and_then(|dt| {
            let secs = dt.timestamp();
            if secs >= 0 {
                Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
            } else {
                None
            }
        })
        .unwrap_or_else(SystemTime::now)
}

/// Send a Sentry event to a client via a scoped Hub.
///
/// Creates a Hub bound to the client so events are routed to the
/// correct DSN without touching the global/thread-local Hub.
fn send_event_to_client(client: &Arc<sentry::Client>, event: SentryEvent<'static>) {
    let hub = sentry::Hub::new(Some(client.clone()), Arc::new(sentry::Scope::default()));
    hub.capture_event(event);
}

/// Long-lived Sentry clients reused across flush cycles.
///
/// Constructed once per DSN configuration and stored alongside the flush loop.
/// This avoids the overhead of creating/tearing down transport worker threads
/// every flush interval.
struct SentryClients {
    oss: Option<Arc<sentry::Client>>,
    enterprise: Option<Arc<sentry::Client>>,
}

impl SentryClients {
    fn new(config: &Config) -> Self {
        let oss = resolve_oss_dsn(config).and_then(|dsn| build_sentry_client(&dsn));
        let enterprise = resolve_enterprise_dsn(config).and_then(|dsn| build_sentry_client(&dsn));
        SentryClients { oss, enterprise }
    }

    /// Flush both clients to ensure all enqueued events are delivered.
    fn flush(&self, timeout: std::time::Duration) {
        if let Some(client) = &self.oss {
            client.flush(Some(timeout));
        }
        if let Some(client) = &self.enterprise {
            client.flush(Some(timeout));
        }
    }
}

fn flush_sentry_and_posthog(
    config: &Config,
    distinct_id: &str,
    sentry_clients: &SentryClients,
    errors: &[ErrorEvent],
    performances: &[PerformanceEvent],
    messages: &[MessageEvent],
) {
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

    let tags = base_sentry_tags(distinct_id);

    // Send errors
    for error in errors {
        let extra = context_to_extra(&error.context);
        let timestamp = parse_timestamp(&error.timestamp);

        let event = SentryEvent {
            message: Some(error.message.clone()),
            level: SentryLevel::Error,
            timestamp,
            platform: "other".into(),
            tags: tags.clone(),
            extra,
            ..Default::default()
        };

        if let Some(client) = &sentry_clients.oss {
            send_event_to_client(client, event.clone());
        }
        if let Some(client) = &sentry_clients.enterprise {
            send_event_to_client(client, event);
        }
    }

    // Send performance events
    for perf in performances {
        let mut extra = context_to_extra(&perf.context);
        extra.insert("operation".to_string(), json!(perf.operation));
        extra.insert("duration_ms".to_string(), json!(perf.duration_ms));
        let timestamp = parse_timestamp(&perf.timestamp);

        let mut perf_tags = tags.clone();
        if let Some(t) = &perf.tags {
            for (key, value) in t {
                perf_tags.insert(key.clone(), value.clone());
            }
        }

        let event = SentryEvent {
            message: Some(format!(
                "Performance: {} ({}ms)",
                perf.operation, perf.duration_ms
            )),
            level: SentryLevel::Info,
            timestamp,
            platform: "other".into(),
            tags: perf_tags,
            extra,
            ..Default::default()
        };

        if let Some(client) = &sentry_clients.oss {
            send_event_to_client(client, event.clone());
        }
        if let Some(client) = &sentry_clients.enterprise {
            send_event_to_client(client, event);
        }
    }

    // Send messages (to Sentry + PostHog)
    for msg in messages {
        let extra = context_to_extra(&msg.context);
        let timestamp = parse_timestamp(&msg.timestamp);

        let event = SentryEvent {
            message: Some(msg.message.clone()),
            level: parse_sentry_level(&msg.level),
            timestamp,
            platform: "other".into(),
            tags: tags.clone(),
            extra,
            ..Default::default()
        };

        if let Some(client) = &sentry_clients.oss {
            send_event_to_client(client, event.clone());
        }
        if let Some(client) = &sentry_clients.enterprise {
            send_event_to_client(client, event);
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

    // Flush the sentry transport to ensure all enqueued events are delivered
    // before this batch is considered complete.
    sentry_clients.flush(std::time::Duration::from_secs(5));
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
