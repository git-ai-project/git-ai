//! Core metrics types for event tracking.
//! All types are exported for use by external crates (e.g., ingestion server).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Current API version for metrics wire format.
pub const METRICS_API_VERSION: u8 = 1;

/// Sparse position-encoded array (HashMap with string keys for positions).
/// Missing keys = not-set, explicit null = null, otherwise value.
pub type SparseArray = HashMap<String, Value>;

/// Event IDs for different metric types.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetricEventId {
    Committed = 1,
    AgentUsage = 2,
    InstallHooks = 3,
    Checkpoint = 4,
}

/// Trait for event-specific values.
pub trait EventValues: Sized {
    fn event_id() -> MetricEventId;
    fn to_sparse(&self) -> SparseArray;
    #[allow(dead_code)]
    fn from_sparse(arr: &SparseArray) -> Self;
}

/// Generic wrapper for any metric event.
/// JSON keys: t=timestamp, e=event_id, v=values, a=attrs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    #[serde(rename = "t")]
    pub timestamp: u32,
    #[serde(rename = "e")]
    pub event_id: u16,
    #[serde(rename = "v")]
    pub values: SparseArray,
    #[serde(rename = "a")]
    pub attrs: SparseArray,
}

impl MetricEvent {
    /// Create a new metric event with current timestamp.
    pub fn new<V: EventValues>(values: &V, attrs: SparseArray) -> Self {
        Self {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32,
            event_id: V::event_id() as u16,
            values: values.to_sparse(),
            attrs,
        }
    }

    /// Create with explicit timestamp (for deserialization/testing).
    #[allow(dead_code)]
    pub fn with_timestamp<V: EventValues>(timestamp: u32, values: &V, attrs: SparseArray) -> Self {
        Self {
            timestamp,
            event_id: V::event_id() as u16,
            values: values.to_sparse(),
            attrs,
        }
    }
}

/// Metrics batch for wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsBatch {
    #[serde(rename = "v")]
    pub version: u8,
    pub events: Vec<MetricEvent>,
}

impl MetricsBatch {
    pub fn new(events: Vec<MetricEvent>) -> Self {
        Self {
            version: METRICS_API_VERSION,
            events,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_batch_serialization() {
        let batch = MetricsBatch::new(vec![]);
        let json = serde_json::to_string(&batch).unwrap();
        assert!(json.contains("\"v\":1"));
        assert!(json.contains("\"events\":[]"));
    }

    #[test]
    fn test_metric_event_serialization() {
        let mut values = SparseArray::new();
        values.insert("0".to_string(), Value::String("test".to_string()));

        let mut attrs = SparseArray::new();
        attrs.insert("0".to_string(), Value::String("version".to_string()));

        let event = MetricEvent {
            timestamp: 1704067200,
            event_id: MetricEventId::Committed as u16,
            values,
            attrs,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"t\":1704067200"));
        assert!(json.contains("\"e\":1"));
        assert!(json.contains("\"v\":{"));
        assert!(json.contains("\"a\":{"));
    }
}
