use crate::authorship::secrets::{redact_secrets_in_text, text_contains_secrets};
use serde_json::Value;

/// Returns true if this key represents a metadata/ID field whose value should NOT be redacted.
fn is_denied_key(key: &str) -> bool {
    if key == "id" || key.ends_with("_id") || key.ends_with("Id") || key.ends_with("ID") {
        return true;
    }
    if key.eq_ignore_ascii_case("uuid")
        || (key.len() > 4 && key.as_bytes()[key.len() - 4..].eq_ignore_ascii_case(b"uuid"))
    {
        return true;
    }
    key.eq_ignore_ascii_case("timestamp")
        || key.eq_ignore_ascii_case("starttime")
        || key.eq_ignore_ascii_case("type")
        || key.eq_ignore_ascii_case("role")
        || key.eq_ignore_ascii_case("model")
        || key.eq_ignore_ascii_case("version")
        || key.eq_ignore_ascii_case("cwd")
        || key.eq_ignore_ascii_case("gitbranch")
        || key.eq_ignore_ascii_case("branch")
        || key.eq_ignore_ascii_case("path")
        || key.eq_ignore_ascii_case("file_path")
        || key.eq_ignore_ascii_case("filepath")
        || key.eq_ignore_ascii_case("producer")
        || key.eq_ignore_ascii_case("name")
        || key.eq_ignore_ascii_case("kind")
        || key.eq_ignore_ascii_case("status")
}

const MAX_REDACTION_DEPTH: usize = 32;

/// Recursively walk a JSON value, applying secret redaction to string leaves
/// whose parent key is not on the denylist.
pub fn redact_json_secrets(value: Value) -> Value {
    redact_json_secrets_inner(value, 0)
}

fn redact_json_secrets_inner(value: Value, depth: usize) -> Value {
    if depth >= MAX_REDACTION_DEPTH {
        return value;
    }
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let redacted_v = if is_denied_key(&k) {
                        v
                    } else {
                        redact_json_secrets_inner(v, depth + 1)
                    };
                    (k, redacted_v)
                })
                .collect(),
        ),
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .map(|v| redact_json_secrets_inner(v, depth + 1))
                .collect(),
        ),
        Value::String(s) => {
            if !text_contains_secrets(&s) {
                Value::String(s)
            } else {
                let (redacted, _) = redact_secrets_in_text(&s);
                Value::String(redacted)
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_redacts_high_entropy_string_in_content_field() {
        let input = json!({
            "content": "Here is my key: sk_test_4eC39HqLyjWDarjtT1zdp7dc please use it"
        });
        let result = redact_json_secrets(input);
        let content = result["content"].as_str().unwrap();
        assert!(!content.contains("sk_test_4eC39HqLyjWDarjtT1zdp7dc"));
        assert!(content.contains("********"));
    }

    #[test]
    fn test_does_not_redact_denied_key_session_id() {
        let secret_looking_id = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        let input = json!({
            "session_id": secret_looking_id
        });
        let result = redact_json_secrets(input);
        assert_eq!(result["session_id"].as_str().unwrap(), secret_looking_id);
    }

    #[test]
    fn test_does_not_redact_denied_key_camel_case() {
        let secret_looking_id = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        let input = json!({
            "parentId": secret_looking_id,
            "callID": secret_looking_id,
            "id": secret_looking_id
        });
        let result = redact_json_secrets(input);
        assert_eq!(result["parentId"].as_str().unwrap(), secret_looking_id);
        assert_eq!(result["callID"].as_str().unwrap(), secret_looking_id);
        assert_eq!(result["id"].as_str().unwrap(), secret_looking_id);
    }

    #[test]
    fn test_does_not_redact_type_role_model_version() {
        let input = json!({
            "type": "sk_test_4eC39HqLyjWDarjtT1zdp7dc",
            "role": "sk_test_4eC39HqLyjWDarjtT1zdp7dc",
            "model": "sk_test_4eC39HqLyjWDarjtT1zdp7dc",
            "version": "sk_test_4eC39HqLyjWDarjtT1zdp7dc"
        });
        let result = redact_json_secrets(input);
        let secret = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        assert_eq!(result["type"].as_str().unwrap(), secret);
        assert_eq!(result["role"].as_str().unwrap(), secret);
        assert_eq!(result["model"].as_str().unwrap(), secret);
        assert_eq!(result["version"].as_str().unwrap(), secret);
    }

    #[test]
    fn test_deeply_nested_redaction() {
        let secret = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        let input = json!({
            "message": {
                "content": [{
                    "text": format!("secret: {secret} please use it"),
                    "type": "text_block"
                }]
            }
        });
        let result = redact_json_secrets(input);
        let text = result["message"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains(secret));
        assert!(text.contains("********"));
        assert_eq!(
            result["message"]["content"][0]["type"].as_str().unwrap(),
            "text_block"
        );
    }

    #[test]
    fn test_no_secrets_returns_unchanged() {
        let input = json!({
            "role": "user",
            "content": "Hello, how are you?",
            "timestamp": "2026-05-19T10:00:00Z"
        });
        let result = redact_json_secrets(input.clone());
        assert_eq!(result, input);
    }

    #[test]
    fn test_array_elements_are_redacted() {
        let input = json!(["normal text", "secret: sk_live_4eC39HqLyjWDarjtT1zdp7dc"]);
        let result = redact_json_secrets(input);
        let arr = result.as_array().unwrap();
        assert_eq!(arr[0].as_str().unwrap(), "normal text");
        assert!(
            !arr[1]
                .as_str()
                .unwrap()
                .contains("sk_live_4eC39HqLyjWDarjtT1zdp7dc")
        );
        assert!(arr[1].as_str().unwrap().contains("********"));
    }

    #[test]
    fn test_denied_key_skips_entire_subtree() {
        // If a key is denied, its entire value subtree should be skipped
        let secret = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        let input = json!({
            "event_id": {
                "nested": secret
            }
        });
        let result = redact_json_secrets(input);
        assert_eq!(result["event_id"]["nested"].as_str().unwrap(), secret);
    }

    #[test]
    fn test_is_denied_key_patterns() {
        // Should deny
        assert!(is_denied_key("id"));
        assert!(is_denied_key("session_id"));
        assert!(is_denied_key("event_id"));
        assert!(is_denied_key("parentId"));
        assert!(is_denied_key("callID"));
        assert!(is_denied_key("uuid"));
        assert!(is_denied_key("parentUuid"));
        assert!(is_denied_key("messageUUID"));
        assert!(is_denied_key("timestamp"));
        assert!(is_denied_key("startTime"));
        assert!(is_denied_key("type"));
        assert!(is_denied_key("role"));
        assert!(is_denied_key("model"));
        assert!(is_denied_key("version"));
        assert!(is_denied_key("cwd"));
        assert!(is_denied_key("gitBranch"));
        assert!(is_denied_key("branch"));
        assert!(is_denied_key("path"));
        assert!(is_denied_key("file_path"));
        assert!(is_denied_key("filePath"));
        assert!(is_denied_key("producer"));
        assert!(is_denied_key("name"));
        assert!(is_denied_key("kind"));
        assert!(is_denied_key("status"));

        // Should NOT deny (these are content-like fields)
        assert!(!is_denied_key("content"));
        assert!(!is_denied_key("text"));
        assert!(!is_denied_key("message"));
        assert!(!is_denied_key("output"));
        assert!(!is_denied_key("value"));
        assert!(!is_denied_key("valid")); // ends in "id" letters but not a real ID field
    }

    #[test]
    fn test_depth_limit_stops_recursion() {
        let secret = "sk_test_4eC39HqLyjWDarjtT1zdp7dc";
        // Build a deeply nested structure exceeding MAX_REDACTION_DEPTH
        let mut value = json!(secret);
        for _ in 0..40 {
            value = json!({ "nested": value });
        }
        let result = redact_json_secrets(value);
        // Walk down to the deepest level — the secret should be preserved
        // because we stopped recursing before reaching it
        let mut cursor = &result;
        for _ in 0..40 {
            cursor = &cursor["nested"];
        }
        assert_eq!(cursor.as_str().unwrap(), secret);
    }
}
