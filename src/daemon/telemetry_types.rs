use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub const METRICS_API_VERSION: u8 = 1;
pub const DEFAULT_API_BASE_URL: &str = "https://usegitai.com";
pub const MAX_CAS_OBJECTS_PER_UPLOAD: usize = 50;

/// Sparse position-encoded array (HashMap with string keys for positions).
pub type SparseArray = HashMap<String, Value>;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetricEventId {
    Committed = 1,
    AgentUsage = 2,
    InstallHooks = 3,
    Checkpoint = 4,
    SessionEvent = 5,
}

/// Wire format: { "t": timestamp, "e": event_id, "v": values, "a": attrs }
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
    pub fn new(event_id: MetricEventId, values: SparseArray, attrs: SparseArray) -> Self {
        Self {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32,
            event_id: event_id as u16,
            values,
            attrs,
        }
    }
}

/// Wire format: { "v": 1, "events": [...] }
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

/// Single CAS object for upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasObject {
    pub content: Value,
    pub hash: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

/// Request body for POST /worker/cas/upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasUploadRequest {
    pub objects: Vec<CasObject>,
}

/// Result for a single CAS object upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasUploadResult {
    pub hash: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from POST /worker/cas/upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasUploadResponse {
    pub results: Vec<CasUploadResult>,
    pub success_count: usize,
    pub failure_count: usize,
}

/// Response from POST /worker/metrics/upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsUploadResponse {
    pub errors: Vec<MetricsUploadError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsUploadError {
    pub index: usize,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_batch_serialization_matches_v1() {
        let batch = MetricsBatch::new(vec![]);
        let json = serde_json::to_string(&batch).unwrap();
        assert!(json.contains("\"v\":1"));
        assert!(json.contains("\"events\":[]"));
    }

    #[test]
    fn metric_event_serialization_uses_compact_keys() {
        let mut values = SparseArray::new();
        values.insert("0".to_string(), Value::String("test".to_string()));

        let mut attrs = SparseArray::new();
        attrs.insert("0".to_string(), Value::String("2.0.0".to_string()));

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

    #[test]
    fn metric_event_deserialization() {
        let json = r#"{"t":1704067200,"e":2,"v":{"0":"test"},"a":{"0":"1.0.0"}}"#;
        let event: MetricEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.timestamp, 1704067200);
        assert_eq!(event.event_id, 2);
    }

    #[test]
    fn cas_object_empty_metadata_not_serialized() {
        let obj = CasObject {
            content: serde_json::json!({"data": "test"}),
            hash: "abc123".to_string(),
            metadata: HashMap::new(),
        };
        let json = serde_json::to_string(&obj).unwrap();
        assert!(!json.contains("metadata"));
    }

    #[test]
    fn cas_upload_request_serialization() {
        let request = CasUploadRequest {
            objects: vec![CasObject {
                content: serde_json::json!({"test": 1}),
                hash: "deadbeef".to_string(),
                metadata: HashMap::new(),
            }],
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"objects\""));
        assert!(json.contains("deadbeef"));
    }

    #[test]
    fn event_id_values_match_v1() {
        assert_eq!(MetricEventId::Committed as u16, 1);
        assert_eq!(MetricEventId::AgentUsage as u16, 2);
        assert_eq!(MetricEventId::InstallHooks as u16, 3);
        assert_eq!(MetricEventId::Checkpoint as u16, 4);
        assert_eq!(MetricEventId::SessionEvent as u16, 5);
    }
}
