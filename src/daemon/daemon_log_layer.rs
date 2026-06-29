//! Tracing layer that forwards daemon log events to the telemetry worker.

use crate::api::types::{DaemonLogEvent, DaemonLogFieldValue, DaemonLogKind, DaemonLogLevel};
use crate::authorship::secrets::redact_secrets_in_text;
use crate::config::Config;
use std::collections::BTreeMap;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

const MAX_MESSAGE_LENGTH: usize = 16_000;
const MAX_TARGET_LENGTH: usize = 512;
const MAX_FIELD_KEY_LENGTH: usize = 256;
const MAX_FIELD_VALUE_LENGTH: usize = 4096;

/// Captures tracing events for best-effort daemon diagnostics upload.
pub struct DaemonLogUploadLayer;

struct DaemonLogVisitor {
    message: String,
    fields: BTreeMap<String, DaemonLogFieldValue>,
}

impl DaemonLogVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: BTreeMap::new(),
        }
    }

    fn record_string(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = sanitize_log_string(&value, MAX_MESSAGE_LENGTH);
            return;
        }

        self.record_field_value(field, DaemonLogFieldValue::String(value));
    }

    fn record_field_value(&mut self, field: &Field, value: DaemonLogFieldValue) {
        self.record_named_field_value(field.name(), value);
    }

    fn record_named_field_value(&mut self, name: &str, value: DaemonLogFieldValue) {
        let key = truncate_string(name, MAX_FIELD_KEY_LENGTH);
        let value = sanitize_field_value(value);
        self.fields.insert(key, value);
    }
}

impl Visit for DaemonLogVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_string(field, format!("{:?}", value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_string(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_field_value(field, DaemonLogFieldValue::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_field_value(field, DaemonLogFieldValue::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_field_value(field, DaemonLogFieldValue::from(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_string(field, value.to_string());
    }
}

impl<S: Subscriber> Layer<S> for DaemonLogUploadLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !Config::get().get_feature_flags().daemon_log_upload {
            return;
        }

        let metadata = event.metadata();
        let mut visitor = DaemonLogVisitor::new();
        event.record(&mut visitor);

        if let Some(file) = metadata.file() {
            visitor.record_named_field_value("file", DaemonLogFieldValue::from(file));
        }
        if let Some(line) = metadata.line() {
            visitor.record_named_field_value("line", DaemonLogFieldValue::from(u64::from(line)));
        }
        if let Some(module_path) = metadata.module_path() {
            visitor.record_named_field_value("module_path", DaemonLogFieldValue::from(module_path));
        }

        let log_event = DaemonLogEvent {
            id: Some(crate::uuid::generate_v4()),
            kind: DaemonLogKind::Log,
            timestamp: chrono::Utc::now().to_rfc3339(),
            level: daemon_log_level_from_tracing(metadata.level()),
            target: Some(sanitize_log_string(metadata.target(), MAX_TARGET_LENGTH)),
            message: visitor.message,
            fields: visitor.fields,
            repo_url: None,
            git_ai_version: None,
        };

        crate::daemon::telemetry_worker::submit_daemon_internal_daemon_logs(vec![log_event]);
    }
}

fn daemon_log_level_from_tracing(level: &Level) -> DaemonLogLevel {
    match *level {
        Level::TRACE => DaemonLogLevel::Trace,
        Level::DEBUG => DaemonLogLevel::Debug,
        Level::INFO => DaemonLogLevel::Info,
        Level::WARN => DaemonLogLevel::Warn,
        Level::ERROR => DaemonLogLevel::Error,
    }
}

fn sanitize_field_value(value: DaemonLogFieldValue) -> DaemonLogFieldValue {
    match value {
        DaemonLogFieldValue::String(raw) => {
            DaemonLogFieldValue::String(sanitize_log_string(&raw, MAX_FIELD_VALUE_LENGTH))
        }
        other => other,
    }
}

fn sanitize_log_string(value: &str, max_len: usize) -> String {
    let (redacted, _) = redact_secrets_in_text(value);
    truncate_string(&redacted, max_len)
}

fn truncate_string(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }

    value.chars().take(max_len).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_log_string_redacts_and_truncates() {
        let secret = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        let value = format!("token={secret}");

        let sanitized = sanitize_log_string(&value, 12);

        assert_eq!(sanitized.chars().count(), 12);
        assert!(!sanitized.contains(secret));
    }

    #[test]
    fn visitor_collects_primitive_fields() {
        let mut visitor = DaemonLogVisitor::new();
        visitor.record_named_field_value("repo", DaemonLogFieldValue::from("example"));
        visitor.record_named_field_value("count", DaemonLogFieldValue::from(3_u64));
        visitor.record_named_field_value("ok", DaemonLogFieldValue::from(true));

        assert!(visitor.fields.contains_key("repo"));
        assert_eq!(
            visitor.fields.get("count"),
            Some(&DaemonLogFieldValue::from(3_u64))
        );
        assert_eq!(
            visitor.fields.get("ok"),
            Some(&DaemonLogFieldValue::from(true))
        );
    }
}
