//! Custom tracing Layer that forwards ERROR-level events to Sentry
//! via the existing daemon telemetry worker pipeline.

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use crate::daemon::daemon_log_layer::{
    MAX_FIELD_VALUE_LENGTH, MAX_FIELDS_PER_EVENT, MAX_MESSAGE_LENGTH, bounded_copy,
    bounded_debug_string, sanitize_log_string,
};

/// A tracing Layer that intercepts ERROR-level events and routes them
/// to the daemon's telemetry worker as `TelemetryEnvelope::Error` events,
/// which get forwarded to both enterprise and OSS Sentry DSNs.
pub struct SentryLayer;

struct MessageVisitor {
    message: String,
    fields: serde_json::Map<String, serde_json::Value>,
}

impl MessageVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: serde_json::Map::new(),
        }
    }

    fn record_string(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = sanitize_log_string(&value, MAX_MESSAGE_LENGTH);
        } else if self.fields.len() < MAX_FIELDS_PER_EVENT || self.fields.contains_key(field.name())
        {
            self.fields.insert(
                field.name().to_string(),
                serde_json::Value::String(sanitize_log_string(&value, MAX_FIELD_VALUE_LENGTH)),
            );
        }
    }

    fn can_record_field(&self, field: &Field) -> bool {
        self.fields.len() < MAX_FIELDS_PER_EVENT || self.fields.contains_key(field.name())
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let max_len = if field.name() == "message" {
            MAX_MESSAGE_LENGTH
        } else {
            MAX_FIELD_VALUE_LENGTH
        };
        self.record_string(field, bounded_debug_string(value, max_len));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let max_len = if field.name() == "message" {
            MAX_MESSAGE_LENGTH
        } else {
            MAX_FIELD_VALUE_LENGTH
        };
        self.record_string(field, bounded_copy(value, max_len));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if self.can_record_field(field) {
            self.fields.insert(
                field.name().to_string(),
                serde_json::Value::Number(value.into()),
            );
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if self.can_record_field(field) {
            self.fields.insert(
                field.name().to_string(),
                serde_json::Value::Number(value.into()),
            );
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if self.can_record_field(field) {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::Bool(value));
        }
    }
}

impl<S: Subscriber> Layer<S> for SentryLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if *event.metadata().level() != Level::ERROR {
            return;
        }

        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);

        let context = if visitor.fields.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(visitor.fields))
        };

        let envelope = crate::daemon::control_api::TelemetryEnvelope::Error {
            timestamp: chrono::Utc::now().to_rfc3339(),
            message: visitor.message,
            context,
        };

        crate::daemon::telemetry_worker::submit_daemon_internal_telemetry(vec![envelope]);
    }
}
