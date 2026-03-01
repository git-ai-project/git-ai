use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct TelemetryPayloadView {
    pub duration_ms: Option<u64>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub failure_type: Option<String>,
    pub mcp_server: Option<String>,
    pub mcp_transport: Option<String>,
    pub mcp_tool_name: Option<String>,
    pub status: Option<String>,
    pub reason: Option<String>,
    pub subagent_id: Option<String>,
    pub subagent_type: Option<String>,
    pub result_char_count: Option<u32>,
    pub response_char_count: Option<u32>,
    pub prompt_char_count: Option<u32>,
    pub attachment_count: Option<u32>,
    pub mode: Option<String>,
    pub dedupe_key: Option<String>,
}

impl TelemetryPayloadView {
    pub fn from_payload(payload: &HashMap<String, String>) -> Self {
        Self::from_payload_with_dedupe_fallback(payload, None)
    }

    pub fn from_payload_with_dedupe_fallback(
        payload: &HashMap<String, String>,
        fallback: Option<&str>,
    ) -> Self {
        Self {
            duration_ms: parse_u64(payload, "duration_ms"),
            tool_name: str_field(payload, "tool_name"),
            tool_use_id: str_field(payload, "tool_use_id"),
            failure_type: str_field(payload, "failure_type"),
            mcp_server: str_field(payload, "mcp_server"),
            mcp_transport: str_field(payload, "mcp_transport"),
            mcp_tool_name: str_field(payload, "mcp_tool_name")
                .or_else(|| str_field(payload, "tool_name")),
            status: str_field(payload, "status"),
            reason: str_field(payload, "reason"),
            subagent_id: str_field(payload, "subagent_id"),
            subagent_type: str_field(payload, "subagent_type"),
            result_char_count: parse_u32(payload, "result_char_count"),
            response_char_count: parse_u32(payload, "response_char_count"),
            prompt_char_count: parse_u32(payload, "prompt_char_count"),
            attachment_count: parse_u32(payload, "attachment_count"),
            mode: str_field(payload, "mode"),
            dedupe_key: payload
                .get("generation_id")
                .or_else(|| payload.get("tool_use_id"))
                .or_else(|| payload.get("message_id"))
                .map(String::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or(fallback)
                .map(str::to_string),
        }
    }
}

fn str_field(payload: &HashMap<String, String>, key: &str) -> Option<String> {
    payload
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_u32(payload: &HashMap<String, String>, key: &str) -> Option<u32> {
    payload
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .and_then(|v| v.parse::<u32>().ok())
}

fn parse_u64(payload: &HashMap<String, String>, key: &str) -> Option<u64> {
    payload
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .and_then(|v| v.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::TelemetryPayloadView;
    use std::collections::HashMap;

    #[test]
    fn test_from_payload_trims_and_discards_empty_fields() {
        let mut payload = HashMap::new();
        payload.insert("tool_name".to_string(), "  ".to_string());
        payload.insert("reason".to_string(), "  denied  ".to_string());
        payload.insert("duration_ms".to_string(), " 42 ".to_string());
        payload.insert("generation_id".to_string(), "   ".to_string());

        let view = TelemetryPayloadView::from_payload_with_dedupe_fallback(&payload, Some("fb"));

        assert_eq!(view.tool_name, None);
        assert_eq!(view.reason.as_deref(), Some("denied"));
        assert_eq!(view.duration_ms, Some(42));
        assert_eq!(view.dedupe_key.as_deref(), Some("fb"));
    }
}
