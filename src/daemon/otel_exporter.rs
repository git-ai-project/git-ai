//! OpenTelemetry (OTLP/HTTP) metrics exporter.
//!
//! Translates git-ai's internal [`MetricEvent`]s into the OTLP metrics data model and POSTs
//! them as OTLP/HTTP JSON to a user-configured backend (OTel Collector, Grafana, Honeycomb,
//! Datadog, …). Enabled whenever [`Config::otel_metrics_endpoint`] resolves to `Some`.
//!
//! Export is best-effort and fire-and-forget: failures are logged and dropped (no SQLite
//! buffering, unlike the first-party metrics API) so a misconfigured backend can't grow an
//! unbounded backlog.
//!
//! All six event kinds are exported via a universal `gitai.events` counter; events that carry
//! meaningful numeric values additionally emit named counters (e.g. `gitai.commit.ai_additions`).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::config::Config;
use crate::metrics::events::{checkpoint_pos, install_hooks_pos};
use crate::metrics::types::SparseArray;
use crate::metrics::{
    CheckpointValues, CommittedValues, EventAttributes, InstallHooksValues, MetricEvent, PosEncoded,
};

const SCOPE_NAME: &str = "git-ai";

/// Flush a batch of metric events to the configured OTLP backend.
///
/// No-op when OTLP export is disabled or the batch produces no metrics.
pub fn flush_otel_metrics(config: &Config, events: &[MetricEvent]) {
    let endpoint = match config.otel_metrics_endpoint() {
        Some(endpoint) => endpoint,
        None => return,
    };

    let document = match build_otlp_metrics_document(config, events) {
        Some(document) => document,
        None => return,
    };

    let body = match serde_json::to_string(&document) {
        Ok(body) => body,
        Err(e) => {
            tracing::warn!(%e, "otel: failed to serialize OTLP metrics document");
            return;
        }
    };

    let agent = crate::http::build_agent(Some(30));
    let mut request = agent
        .post(&endpoint)
        .set("Content-Type", "application/json");
    if let Some(headers) = config.otel().headers.as_ref() {
        for (key, value) in headers {
            request = request.set(key, value);
        }
    }

    match crate::http::send_with_body(request, &body) {
        Ok(resp) if (200..300).contains(&resp.status_code) => {
            tracing::debug!(status = resp.status_code, "otel: exported metrics");
        }
        Ok(resp) => {
            tracing::warn!(
                status = resp.status_code,
                "otel: backend rejected metrics export"
            );
        }
        Err(e) => {
            tracing::warn!(%e, "otel: failed to POST metrics to backend");
        }
    }
}

/// Build the OTLP/HTTP JSON metrics document for a batch of events.
///
/// Returns `None` when the batch is empty (no request should be sent).
pub fn build_otlp_metrics_document(config: &Config, events: &[MetricEvent]) -> Option<Value> {
    if events.is_empty() {
        return None;
    }

    // Group data points by metric name (BTreeMap keeps output deterministic for snapshots).
    let mut metric_points: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();

    for event in events {
        let common = common_attributes(&event.attrs);
        let time_unix_nano = (event.timestamp as u64).saturating_mul(1_000_000_000);

        let (extra_counter_attrs, numeric_metrics) = event_specific(event);

        // Universal per-event counter.
        let mut counter_attrs = common.clone();
        counter_attrs.push(attribute(
            "event.name",
            any_string(event_name(event.event_id)),
        ));
        counter_attrs.extend(extra_counter_attrs.iter().cloned());
        metric_points
            .entry("gitai.events")
            .or_default()
            .push(data_point(time_unix_nano, 1, counter_attrs));

        // Event-specific numeric metrics.
        for metric in numeric_metrics {
            let mut attrs = common.clone();
            attrs.extend(extra_counter_attrs.iter().cloned());
            attrs.extend(metric.attrs);
            metric_points
                .entry(metric.name)
                .or_default()
                .push(data_point(time_unix_nano, metric.value, attrs));
        }
    }

    let metrics: Vec<Value> = metric_points
        .into_iter()
        .map(|(name, data_points)| sum_metric(name, data_points))
        .collect();

    Some(json!({
        "resourceMetrics": [{
            "resource": { "attributes": resource_attributes(config) },
            "scopeMetrics": [{
                "scope": {
                    "name": SCOPE_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "metrics": metrics,
            }],
        }],
    }))
}

/// A named numeric metric extracted from an event's values.
struct NumericMetric {
    name: &'static str,
    value: i64,
    attrs: Vec<Value>,
}

/// Extract event-specific counter attributes and numeric metrics for a single event.
fn event_specific(event: &MetricEvent) -> (Vec<Value>, Vec<NumericMetric>) {
    match event.event_id {
        x if x == crate::metrics::types::MetricEventId::Committed as u16 => {
            let v = <CommittedValues as PosEncoded>::from_sparse(&event.values);
            let mut metrics = Vec::new();
            push_u32(
                &mut metrics,
                "gitai.commit.human_additions",
                v.human_additions,
            );
            push_u32(
                &mut metrics,
                "gitai.commit.diff_added_lines",
                v.git_diff_added_lines,
            );
            push_u32(
                &mut metrics,
                "gitai.commit.diff_deleted_lines",
                v.git_diff_deleted_lines,
            );
            // Parallel arrays: index 0 is the aggregate ("all") across tools/models.
            if let Some(Some(values)) = v.ai_additions.as_ref()
                && let Some(total) = values.first()
            {
                metrics.push(NumericMetric {
                    name: "gitai.commit.ai_additions",
                    value: *total as i64,
                    attrs: Vec::new(),
                });
            }
            if let Some(Some(values)) = v.ai_accepted.as_ref()
                && let Some(total) = values.first()
            {
                metrics.push(NumericMetric {
                    name: "gitai.commit.ai_accepted",
                    value: *total as i64,
                    attrs: Vec::new(),
                });
            }
            (Vec::new(), metrics)
        }
        x if x == crate::metrics::types::MetricEventId::Checkpoint as u16 => {
            let v = <CheckpointValues as PosEncoded>::from_sparse(&event.values);
            let mut extra = Vec::new();
            if let Some(kind) = sparse_string(&event.values, checkpoint_pos::KIND) {
                extra.push(attribute("checkpoint.kind", any_string(&kind)));
            }
            if let Some(edit_kind) = sparse_string(&event.values, checkpoint_pos::EDIT_KIND) {
                extra.push(attribute("checkpoint.edit_kind", any_string(&edit_kind)));
            }
            let mut metrics = Vec::new();
            push_u32(&mut metrics, "gitai.checkpoint.lines_added", v.lines_added);
            push_u32(
                &mut metrics,
                "gitai.checkpoint.lines_deleted",
                v.lines_deleted,
            );
            push_u32(
                &mut metrics,
                "gitai.checkpoint.lines_added_sloc",
                v.lines_added_sloc,
            );
            push_u32(
                &mut metrics,
                "gitai.checkpoint.lines_deleted_sloc",
                v.lines_deleted_sloc,
            );
            (extra, metrics)
        }
        x if x == crate::metrics::types::MetricEventId::InstallHooks as u16 => {
            let _ = <InstallHooksValues as PosEncoded>::from_sparse(&event.values);
            let mut extra = Vec::new();
            if let Some(tool_id) = sparse_string(&event.values, install_hooks_pos::TOOL_ID) {
                extra.push(attribute("install.tool_id", any_string(&tool_id)));
            }
            if let Some(status) = sparse_string(&event.values, install_hooks_pos::STATUS) {
                extra.push(attribute("install.status", any_string(&status)));
            }
            (extra, Vec::new())
        }
        // Committed extracts numeric scalars from named positions; the constants below keep the
        // mapping legible at the call site above.
        _ => (Vec::new(), Vec::new()),
    }
}

/// Convert the shared [`EventAttributes`] into OTLP key/value attributes (present fields only).
fn common_attributes(attrs: &SparseArray) -> Vec<Value> {
    let decoded = EventAttributes::from_sparse(attrs);
    let mut out = Vec::new();

    push_string_attr(&mut out, "gitai.version", &decoded.git_ai_version);
    push_string_attr(&mut out, "gitai.repo_url", &decoded.repo_url);
    push_string_attr(&mut out, "gitai.author", &decoded.author);
    push_string_attr(&mut out, "gitai.commit_sha", &decoded.commit_sha);
    push_string_attr(&mut out, "gitai.base_commit_sha", &decoded.base_commit_sha);
    push_string_attr(&mut out, "gitai.branch", &decoded.branch);
    push_string_attr(&mut out, "gitai.tool", &decoded.tool);
    push_string_attr(&mut out, "gitai.model", &decoded.model);
    push_string_attr(&mut out, "gitai.session_id", &decoded.session_id);
    push_string_attr(&mut out, "gitai.trace_id", &decoded.trace_id);
    push_string_attr(
        &mut out,
        "gitai.parent_session_id",
        &decoded.parent_session_id,
    );
    push_string_attr(
        &mut out,
        "gitai.external_session_id",
        &decoded.external_session_id,
    );
    push_string_attr(
        &mut out,
        "gitai.external_parent_session_id",
        &decoded.external_parent_session_id,
    );

    // custom_attributes is a JSON object string; flatten its scalar entries (sorted for stability).
    if let Some(Some(raw)) = decoded.custom_attributes.as_ref()
        && let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<Value>(raw)
    {
        for (key, value) in map {
            if let Some(any) = scalar_any_value(&value) {
                out.push(attribute(&format!("gitai.custom.{key}"), any));
            }
        }
    }

    out
}

/// Build the OTLP Resource attributes (service identity + host + user-configured attributes).
fn resource_attributes(config: &Config) -> Vec<Value> {
    let mut out = vec![
        attribute("service.name", any_string(config.otel_service_name())),
        attribute("service.version", any_string(env!("CARGO_PKG_VERSION"))),
        attribute("os.type", any_string(std::env::consts::OS)),
        attribute("host.arch", any_string(std::env::consts::ARCH)),
    ];
    if let Some(extra) = config.otel().resource_attributes.as_ref() {
        // Sorted for deterministic output.
        for (key, value) in extra.iter().collect::<BTreeMap<_, _>>() {
            out.push(attribute(key, any_string(value)));
        }
    }
    out
}

fn event_name(event_id: u16) -> &'static str {
    use crate::metrics::types::MetricEventId::*;
    if event_id == Committed as u16 {
        "committed"
    } else if event_id == AgentUsage as u16 {
        "agent_usage"
    } else if event_id == InstallHooks as u16 {
        "install_hooks"
    } else if event_id == Checkpoint as u16 {
        "checkpoint"
    } else if event_id == SessionEvent as u16 {
        "session_event"
    } else if event_id == OtelTrace as u16 {
        "otel_trace"
    } else {
        "unknown"
    }
}

// --- OTLP JSON helpers -----------------------------------------------------

fn sum_metric(name: &str, data_points: Vec<Value>) -> Value {
    json!({
        "name": name,
        "unit": "1",
        "sum": {
            // 1 = AGGREGATION_TEMPORALITY_DELTA
            "aggregationTemporality": 1,
            "isMonotonic": true,
            "dataPoints": data_points,
        },
    })
}

fn data_point(time_unix_nano: u64, value: i64, attributes: Vec<Value>) -> Value {
    json!({
        "timeUnixNano": time_unix_nano.to_string(),
        // OTLP/JSON encodes 64-bit integers as strings.
        "asInt": value.to_string(),
        "attributes": attributes,
    })
}

fn attribute(key: &str, value: Value) -> Value {
    json!({ "key": key, "value": value })
}

fn any_string(value: &str) -> Value {
    json!({ "stringValue": value })
}

fn any_int(value: i64) -> Value {
    json!({ "intValue": value.to_string() })
}

fn scalar_any_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => Some(any_string(s)),
        Value::Bool(b) => Some(json!({ "boolValue": b })),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(any_int(i))
            } else {
                n.as_f64().map(|f| json!({ "doubleValue": f }))
            }
        }
        _ => None,
    }
}

fn push_string_attr(out: &mut Vec<Value>, key: &str, field: &Option<Option<String>>) {
    if let Some(Some(value)) = field {
        out.push(attribute(key, any_string(value)));
    }
}

fn push_u32(out: &mut Vec<NumericMetric>, name: &'static str, field: Option<Option<u32>>) {
    if let Some(Some(value)) = field {
        out.push(NumericMetric {
            name,
            value: value as i64,
            attrs: Vec::new(),
        });
    }
}

fn sparse_string(arr: &SparseArray, pos: usize) -> Option<String> {
    match arr.get(&pos.to_string()) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MetricEvent;

    fn committed_event() -> MetricEvent {
        let values = CommittedValues::new()
            .human_additions(5)
            .git_diff_added_lines(20)
            .git_diff_deleted_lines(3)
            .tool_model_pairs(vec!["all".to_string(), "claude-code::sonnet".to_string()])
            .ai_additions(vec![15, 15])
            .ai_accepted(vec![12, 12]);
        let attrs = EventAttributes::with_version("9.9.9-test")
            .repo_url("https://github.com/acme/widgets")
            .author("dev@example.com")
            .commit_sha("abc123")
            .branch("main")
            .tool("claude-code")
            .model("sonnet")
            .session_id("sess-1");
        MetricEvent::with_timestamp(1_700_000_000, &values, attrs.to_sparse())
    }

    #[test]
    fn empty_batch_produces_no_document() {
        let config = Config::get();
        assert!(build_otlp_metrics_document(config, &[]).is_none());
    }

    #[test]
    fn committed_event_maps_to_otlp_metrics() {
        let config = Config::get();
        let doc = build_otlp_metrics_document(config, &[committed_event()])
            .expect("document should be built");

        let metrics = doc["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .expect("metrics array");
        let names: Vec<&str> = metrics.iter().filter_map(|m| m["name"].as_str()).collect();

        assert!(names.contains(&"gitai.events"));
        assert!(names.contains(&"gitai.commit.human_additions"));
        assert!(names.contains(&"gitai.commit.ai_additions"));
        assert!(names.contains(&"gitai.commit.ai_accepted"));

        // The universal counter carries the event.name and decoded attributes.
        let counter = metrics
            .iter()
            .find(|m| m["name"] == "gitai.events")
            .expect("gitai.events present");
        let dp = &counter["sum"]["dataPoints"][0];
        assert_eq!(dp["asInt"], "1");
        assert_eq!(dp["timeUnixNano"], "1700000000000000000");
        let attr_keys: Vec<&str> = dp["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|a| a["key"].as_str())
            .collect();
        assert!(attr_keys.contains(&"event.name"));
        assert!(attr_keys.contains(&"gitai.tool"));

        // ai_additions takes the aggregate (index 0) of the parallel array.
        let ai = metrics
            .iter()
            .find(|m| m["name"] == "gitai.commit.ai_additions")
            .unwrap();
        assert_eq!(ai["sum"]["dataPoints"][0]["asInt"], "15");
    }

    #[test]
    fn resource_carries_service_identity() {
        let config = Config::get();
        let doc =
            build_otlp_metrics_document(config, &[committed_event()]).expect("document built");
        let resource_attrs = doc["resourceMetrics"][0]["resource"]["attributes"]
            .as_array()
            .unwrap();
        let service_name = resource_attrs
            .iter()
            .find(|a| a["key"] == "service.name")
            .expect("service.name present");
        assert_eq!(service_name["value"]["stringValue"], "git-ai");
    }
}
