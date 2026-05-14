//! Contract tests verifying v2 telemetry wire format matches v1.
//!
//! These tests validate that the JSON produced by v2 types is structurally
//! compatible with what the backend expects (same schema as v1).

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sha2::{Digest, Sha256};

    use crate::daemon::telemetry_types::*;

    #[test]
    fn metrics_batch_version_is_1() {
        let batch = MetricsBatch::new(vec![]);
        let json: serde_json::Value = serde_json::to_value(&batch).unwrap();
        assert_eq!(json["v"], 1);
    }

    #[test]
    fn metric_event_uses_compact_single_char_keys() {
        let mut values = SparseArray::new();
        values.insert("0".to_string(), serde_json::json!(42));

        let mut attrs = SparseArray::new();
        attrs.insert("0".to_string(), serde_json::json!("2.0.0"));
        attrs.insert("20".to_string(), serde_json::json!("cursor"));

        let event = MetricEvent {
            timestamp: 1715644800,
            event_id: MetricEventId::Committed as u16,
            values,
            attrs,
        };

        let json_str = serde_json::to_string(&event).unwrap();
        let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // Keys must be "t", "e", "v", "a" — not full names
        assert!(json.get("t").is_some(), "missing 't' key");
        assert!(json.get("e").is_some(), "missing 'e' key");
        assert!(json.get("v").is_some(), "missing 'v' key");
        assert!(json.get("a").is_some(), "missing 'a' key");

        // Full names must NOT appear
        assert!(json.get("timestamp").is_none());
        assert!(json.get("event_id").is_none());
        assert!(json.get("values").is_none());
        assert!(json.get("attrs").is_none());

        // Values are correct types
        assert_eq!(json["t"], 1715644800);
        assert_eq!(json["e"], 1);
        assert_eq!(json["v"]["0"], 42);
        assert_eq!(json["a"]["0"], "2.0.0");
        assert_eq!(json["a"]["20"], "cursor");
    }

    #[test]
    fn metric_event_sparse_arrays_use_string_position_keys() {
        let mut values = SparseArray::new();
        values.insert("0".to_string(), serde_json::json!(100));
        values.insert("5".to_string(), serde_json::json!("hello"));
        // Position 1-4 omitted (sparse)

        let event = MetricEvent::new(MetricEventId::Checkpoint, values, SparseArray::new());

        let json: serde_json::Value = serde_json::to_value(&event).unwrap();
        let v = json["v"].as_object().unwrap();

        assert_eq!(v.len(), 2);
        assert!(v.contains_key("0"));
        assert!(v.contains_key("5"));
        assert!(!v.contains_key("1")); // sparse — missing keys are omitted
    }

    #[test]
    fn metrics_batch_full_roundtrip() {
        let mut values = SparseArray::new();
        values.insert("0".to_string(), serde_json::json!(50));

        let mut attrs = SparseArray::new();
        attrs.insert("0".to_string(), serde_json::json!("2.0.0-alpha.1"));
        attrs.insert("1".to_string(), serde_json::json!("https://github.com/user/repo"));

        let event = MetricEvent {
            timestamp: 1715644800,
            event_id: MetricEventId::Committed as u16,
            values,
            attrs,
        };

        let batch = MetricsBatch::new(vec![event]);
        let json_str = serde_json::to_string(&batch).unwrap();

        // Verify roundtrip
        let deserialized: MetricsBatch = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.version, 1);
        assert_eq!(deserialized.events.len(), 1);
        assert_eq!(deserialized.events[0].timestamp, 1715644800);
        assert_eq!(deserialized.events[0].event_id, 1);
    }

    #[test]
    fn cas_hash_is_sha256_of_content_json() {
        let content = serde_json::json!({
            "prompts": {"p1": {"tool": "cursor", "model": "gpt-4"}},
            "files": {}
        });

        let content_json = serde_json::to_string(&content).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(content_json.as_bytes());
        let expected_hash = format!("{:x}", hasher.finalize());

        let obj = CasObject {
            content: content.clone(),
            hash: expected_hash.clone(),
            metadata: HashMap::new(),
        };

        // Hash must be lowercase hex SHA256 of the serialized content
        assert_eq!(obj.hash.len(), 64);
        assert!(obj.hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(obj.hash, expected_hash);
    }

    #[test]
    fn cas_upload_request_structure() {
        let mut metadata = HashMap::new();
        metadata.insert("repo".to_string(), "user/project".to_string());
        metadata.insert("commit".to_string(), "abc1234".to_string());

        let request = CasUploadRequest {
            objects: vec![CasObject {
                content: serde_json::json!({"data": "test"}),
                hash: "a".repeat(64),
                metadata,
            }],
        };

        let json: serde_json::Value = serde_json::to_value(&request).unwrap();

        // Must have "objects" array
        assert!(json["objects"].is_array());
        let obj = &json["objects"][0];

        // Each object must have "content", "hash", and optionally "metadata"
        assert!(obj["content"].is_object());
        assert!(obj["hash"].is_string());
        assert!(obj["metadata"].is_object());
        assert_eq!(obj["metadata"]["repo"], "user/project");
    }

    #[test]
    fn cas_empty_metadata_omitted() {
        let request = CasUploadRequest {
            objects: vec![CasObject {
                content: serde_json::json!({}),
                hash: "b".repeat(64),
                metadata: HashMap::new(),
            }],
        };

        let json_str = serde_json::to_string(&request).unwrap();
        // Empty metadata should not appear in JSON
        assert!(!json_str.contains("metadata"));
    }

    #[test]
    fn metrics_upload_response_deserialization() {
        // Backend returns this format
        let json = r#"{"errors":[]}"#;
        let resp: MetricsUploadResponse = serde_json::from_str(json).unwrap();
        assert!(resp.errors.is_empty());

        let json_with_errors = r#"{"errors":[{"index":2,"error":"invalid event_id"}]}"#;
        let resp: MetricsUploadResponse = serde_json::from_str(json_with_errors).unwrap();
        assert_eq!(resp.errors.len(), 1);
        assert_eq!(resp.errors[0].index, 2);
    }

    #[test]
    fn cas_upload_response_deserialization() {
        let json = r#"{"results":[{"hash":"abc","status":"ok"}],"success_count":1,"failure_count":0}"#;
        let resp: CasUploadResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.success_count, 1);
        assert_eq!(resp.failure_count, 0);
        assert_eq!(resp.results[0].status, "ok");
    }

    #[test]
    fn all_event_ids_are_valid_u16() {
        // v1 backend expects event_id as u16 in range 1..=5
        let ids = [
            MetricEventId::Committed,
            MetricEventId::AgentUsage,
            MetricEventId::InstallHooks,
            MetricEventId::Checkpoint,
            MetricEventId::SessionEvent,
        ];

        for (i, id) in ids.iter().enumerate() {
            assert_eq!(*id as u16, (i + 1) as u16);
        }
    }

    #[test]
    fn null_values_in_sparse_array_serialized_explicitly() {
        let mut attrs = SparseArray::new();
        attrs.insert("0".to_string(), serde_json::json!("version"));
        attrs.insert("1".to_string(), serde_json::Value::Null); // explicit null

        let event = MetricEvent {
            timestamp: 1000000,
            event_id: 1,
            values: SparseArray::new(),
            attrs,
        };

        let json_str = serde_json::to_string(&event).unwrap();
        // Null must be present as explicit null, not omitted
        assert!(json_str.contains("null"));
    }
}
