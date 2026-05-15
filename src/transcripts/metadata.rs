//! Metadata extraction from transcript events.
//!
//! Pulls out useful fields (model, session ID, event count) from raw JSON events
//! without requiring full deserialization into agent-specific types.

use serde_json::Value;

/// Metadata extracted from a batch of transcript events.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptMetadata {
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub event_count: usize,
}

/// Extract the model identifier from transcript events.
///
/// Looks for a "model" field at various nesting levels common across agents:
/// - Top-level `"model"` field
/// - Inside `"metadata"`, `"message"`, `"request"`, or `"response"` objects
///
/// Returns the first match found scanning from earliest to latest event.
pub fn extract_model(events: &[Value]) -> Option<String> {
    static PARENTS: &[&str] = &["metadata", "message", "request", "response"];

    for event in events {
        // Direct top-level "model" field
        if let Some(m) = get_nonempty_str(event, "model") {
            return Some(m.to_string());
        }

        // Nested in known parent objects
        for parent in PARENTS {
            if let Some(obj) = event.get(*parent)
                && let Some(m) = get_nonempty_str(obj, "model")
            {
                return Some(m.to_string());
            }
        }
    }
    None
}

/// Get a non-empty string field from a JSON value.
fn get_nonempty_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|s| !s.is_empty())
}

/// Extract a session or conversation identifier from transcript events.
///
/// Looks for common field names used across agents:
/// - `"session_id"`, `"sessionId"`
/// - `"conversation_id"`, `"conversationId"`
/// - `"thread_id"`, `"threadId"`
///
/// Returns the first match found.
pub fn extract_session_id(events: &[Value]) -> Option<String> {
    static KEYS: &[&str] = &[
        "session_id",
        "sessionId",
        "conversation_id",
        "conversationId",
        "thread_id",
        "threadId",
    ];

    for event in events {
        for key in KEYS {
            if let Some(v) = get_nonempty_str(event, key) {
                return Some(v.to_string());
            }
        }

        if let Some(meta) = event.get("metadata") {
            for key in KEYS {
                if let Some(v) = get_nonempty_str(meta, key) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Build a `TranscriptMetadata` from a slice of events.
pub fn extract_metadata(events: &[Value]) -> TranscriptMetadata {
    TranscriptMetadata {
        model: extract_model(events),
        session_id: extract_session_id(events),
        event_count: events.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_model_top_level() {
        let events = vec![
            json!({"role": "user", "text": "hello"}),
            json!({"role": "assistant", "model": "claude-3-opus", "text": "hi"}),
        ];
        assert_eq!(extract_model(&events), Some("claude-3-opus".to_string()));
    }

    #[test]
    fn test_extract_model_in_metadata() {
        let events = vec![json!({"metadata": {"model": "gpt-4", "timestamp": 123}})];
        assert_eq!(extract_model(&events), Some("gpt-4".to_string()));
    }

    #[test]
    fn test_extract_model_in_message() {
        let events = vec![json!({"message": {"model": "claude-3-sonnet", "content": "x"}})];
        assert_eq!(extract_model(&events), Some("claude-3-sonnet".to_string()));
    }

    #[test]
    fn test_extract_model_in_response() {
        let events = vec![json!({"response": {"model": "codex-mini", "output": "y"}})];
        assert_eq!(extract_model(&events), Some("codex-mini".to_string()));
    }

    #[test]
    fn test_extract_model_none() {
        let events = vec![json!({"role": "user", "text": "no model here"})];
        assert_eq!(extract_model(&events), None);
    }

    #[test]
    fn test_extract_model_skips_empty() {
        let events = vec![json!({"model": ""}), json!({"model": "real-model"})];
        assert_eq!(extract_model(&events), Some("real-model".to_string()));
    }

    #[test]
    fn test_extract_session_id_direct() {
        let events = vec![json!({"session_id": "sess-abc-123", "role": "user"})];
        assert_eq!(
            extract_session_id(&events),
            Some("sess-abc-123".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_camel_case() {
        let events = vec![json!({"sessionId": "my-session", "data": 1})];
        assert_eq!(extract_session_id(&events), Some("my-session".to_string()));
    }

    #[test]
    fn test_extract_session_id_conversation() {
        let events = vec![json!({"conversation_id": "conv-999"})];
        assert_eq!(extract_session_id(&events), Some("conv-999".to_string()));
    }

    #[test]
    fn test_extract_session_id_thread() {
        let events = vec![json!({"threadId": "thread-42"})];
        assert_eq!(extract_session_id(&events), Some("thread-42".to_string()));
    }

    #[test]
    fn test_extract_session_id_in_metadata() {
        let events = vec![json!({"metadata": {"session_id": "meta-sess"}})];
        assert_eq!(extract_session_id(&events), Some("meta-sess".to_string()));
    }

    #[test]
    fn test_extract_session_id_none() {
        let events = vec![json!({"role": "assistant", "text": "hi"})];
        assert_eq!(extract_session_id(&events), None);
    }

    #[test]
    fn test_extract_metadata_full() {
        let events = vec![
            json!({"session_id": "sess-1", "model": "claude-4"}),
            json!({"role": "assistant", "text": "done"}),
        ];
        let meta = extract_metadata(&events);
        assert_eq!(meta.model, Some("claude-4".to_string()));
        assert_eq!(meta.session_id, Some("sess-1".to_string()));
        assert_eq!(meta.event_count, 2);
    }

    #[test]
    fn test_extract_metadata_empty() {
        let meta = extract_metadata(&[]);
        assert_eq!(meta.model, None);
        assert_eq!(meta.session_id, None);
        assert_eq!(meta.event_count, 0);
    }
}
