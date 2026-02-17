use chrono::DateTime;
use serde::{Deserialize, Serialize};

/// Represents a single message in an AI transcript
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    User {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    Assistant {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    Thinking {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    Plan {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
    ToolUse {
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },
}

impl Message {
    /// Create a user message
    pub fn user(text: String, timestamp: Option<String>) -> Self {
        Message::User { text, timestamp }
    }

    /// Create an assistant message
    pub fn assistant(text: String, timestamp: Option<String>) -> Self {
        Message::Assistant { text, timestamp }
    }

    /// Create a thinking message
    #[allow(dead_code)]
    pub fn thinking(text: String, timestamp: Option<String>) -> Self {
        Message::Thinking { text, timestamp }
    }

    /// Create a plan message
    #[allow(dead_code)]
    pub fn plan(text: String, timestamp: Option<String>) -> Self {
        Message::Plan { text, timestamp }
    }

    /// Create a tool use message
    pub fn tool_use(name: String, input: serde_json::Value) -> Self {
        Message::ToolUse {
            name,
            input,
            timestamp: None,
        }
    }

    /// Get the text content if this is a user or AI text message
    #[allow(dead_code)]
    pub fn text(&self) -> Option<&String> {
        match self {
            Message::User { text, .. }
            | Message::Assistant { text, .. }
            | Message::Thinking { text, .. }
            | Message::Plan { text, .. } => Some(text),
            Message::ToolUse { .. } => None,
        }
    }

    /// Check if this is a tool use message
    #[allow(dead_code)]
    pub fn is_tool_use(&self) -> bool {
        matches!(self, Message::ToolUse { .. })
    }

    /// Get the timestamp if present
    pub fn timestamp(&self) -> Option<&String> {
        match self {
            Message::User { timestamp, .. }
            | Message::Assistant { timestamp, .. }
            | Message::Thinking { timestamp, .. }
            | Message::Plan { timestamp, .. }
            | Message::ToolUse { timestamp, .. } => timestamp.as_ref(),
        }
    }
}

/// Represents a complete AI transcript (collection of messages)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiTranscript {
    pub messages: Vec<Message>,
}

impl AiTranscript {
    /// Create a new empty transcript
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Add a message to the transcript
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Get all messages
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Filter out tool use messages
    #[allow(dead_code)]
    pub fn without_tool_use(&self) -> Self {
        let filtered_messages: Vec<Message> = self
            .messages
            .iter()
            .filter(|msg| !msg.is_tool_use())
            .cloned()
            .collect();

        Self {
            messages: filtered_messages,
        }
    }

    /// Get first message timestamp as Unix i64 (for created_at)
    /// Returns None if no messages or first message has no timestamp
    pub fn first_message_timestamp_unix(&self) -> Option<i64> {
        self.messages
            .first()
            .and_then(|msg| msg.timestamp())
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.timestamp())
    }

    /// Get last message timestamp as Unix i64 (for updated_at)
    /// Returns None if no messages or last message has no timestamp
    pub fn last_message_timestamp_unix(&self) -> Option<i64> {
        self.messages
            .last()
            .and_then(|msg| msg.timestamp())
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.timestamp())
    }
}

impl Default for AiTranscript {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_message_user() {
        let msg = Message::user(
            "Hello".to_string(),
            Some("2024-01-01T00:00:00Z".to_string()),
        );
        match msg {
            Message::User { text, timestamp } => {
                assert_eq!(text, "Hello");
                assert_eq!(timestamp, Some("2024-01-01T00:00:00Z".to_string()));
            }
            _ => panic!("Expected User message"),
        }
    }

    #[test]
    fn test_message_assistant() {
        let msg = Message::assistant(
            "Response".to_string(),
            Some("2024-01-01T00:00:01Z".to_string()),
        );
        match msg {
            Message::Assistant { text, timestamp } => {
                assert_eq!(text, "Response");
                assert_eq!(timestamp, Some("2024-01-01T00:00:01Z".to_string()));
            }
            _ => panic!("Expected Assistant message"),
        }
    }

    #[test]
    fn test_message_thinking() {
        let msg = Message::thinking(
            "Thinking...".to_string(),
            Some("2024-01-01T00:00:02Z".to_string()),
        );
        match msg {
            Message::Thinking { text, timestamp } => {
                assert_eq!(text, "Thinking...");
                assert_eq!(timestamp, Some("2024-01-01T00:00:02Z".to_string()));
            }
            _ => panic!("Expected Thinking message"),
        }
    }

    #[test]
    fn test_message_plan() {
        let msg = Message::plan(
            "Plan step".to_string(),
            Some("2024-01-01T00:00:03Z".to_string()),
        );
        match msg {
            Message::Plan { text, timestamp } => {
                assert_eq!(text, "Plan step");
                assert_eq!(timestamp, Some("2024-01-01T00:00:03Z".to_string()));
            }
            _ => panic!("Expected Plan message"),
        }
    }

    #[test]
    fn test_message_tool_use() {
        let input = json!({"param": "value"});
        let msg = Message::tool_use("read_file".to_string(), input.clone());
        match msg {
            Message::ToolUse {
                name,
                input: tool_input,
                timestamp,
            } => {
                assert_eq!(name, "read_file");
                assert_eq!(tool_input, input);
                assert_eq!(timestamp, None);
            }
            _ => panic!("Expected ToolUse message"),
        }
    }

    #[test]
    fn test_message_text() {
        let user_msg = Message::user("User text".to_string(), None);
        assert_eq!(user_msg.text(), Some(&"User text".to_string()));

        let assistant_msg = Message::assistant("Assistant text".to_string(), None);
        assert_eq!(assistant_msg.text(), Some(&"Assistant text".to_string()));

        let thinking_msg = Message::thinking("Thinking text".to_string(), None);
        assert_eq!(thinking_msg.text(), Some(&"Thinking text".to_string()));

        let plan_msg = Message::plan("Plan text".to_string(), None);
        assert_eq!(plan_msg.text(), Some(&"Plan text".to_string()));

        let tool_msg = Message::tool_use("tool".to_string(), json!({}));
        assert_eq!(tool_msg.text(), None);
    }

    #[test]
    fn test_message_is_tool_use() {
        let user_msg = Message::user("text".to_string(), None);
        assert!(!user_msg.is_tool_use());

        let tool_msg = Message::tool_use("tool".to_string(), json!({}));
        assert!(tool_msg.is_tool_use());
    }

    #[test]
    fn test_message_timestamp() {
        let ts = Some("2024-01-01T00:00:00Z".to_string());
        let msg = Message::user("text".to_string(), ts.clone());
        assert_eq!(msg.timestamp(), Some(&"2024-01-01T00:00:00Z".to_string()));

        let msg_no_ts = Message::user("text".to_string(), None);
        assert_eq!(msg_no_ts.timestamp(), None);
    }

    #[test]
    fn test_ai_transcript_new() {
        let transcript = AiTranscript::new();
        assert!(transcript.messages.is_empty());
    }

    #[test]
    fn test_ai_transcript_add_message() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("Hello".to_string(), None));
        transcript.add_message(Message::assistant("Hi".to_string(), None));

        assert_eq!(transcript.messages.len(), 2);
    }

    #[test]
    fn test_ai_transcript_messages() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("msg1".to_string(), None));
        transcript.add_message(Message::assistant("msg2".to_string(), None));

        let messages = transcript.messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text(), Some(&"msg1".to_string()));
        assert_eq!(messages[1].text(), Some(&"msg2".to_string()));
    }

    #[test]
    fn test_ai_transcript_without_tool_use() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("user msg".to_string(), None));
        transcript.add_message(Message::tool_use("tool".to_string(), json!({})));
        transcript.add_message(Message::assistant("assistant msg".to_string(), None));

        let filtered = transcript.without_tool_use();
        assert_eq!(filtered.messages.len(), 2);
        assert!(filtered.messages.iter().all(|msg| !msg.is_tool_use()));
    }

    #[test]
    fn test_ai_transcript_first_message_timestamp_unix() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user(
            "first".to_string(),
            Some("2024-01-01T12:00:00+00:00".to_string()),
        ));
        transcript.add_message(Message::assistant(
            "second".to_string(),
            Some("2024-01-01T12:30:00+00:00".to_string()),
        ));

        let first_ts = transcript.first_message_timestamp_unix();
        assert!(first_ts.is_some());
        // 2024-01-01T12:00:00Z is 1704110400
        assert_eq!(first_ts.unwrap(), 1704110400);
    }

    #[test]
    fn test_ai_transcript_last_message_timestamp_unix() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user(
            "first".to_string(),
            Some("2024-01-01T12:00:00+00:00".to_string()),
        ));
        transcript.add_message(Message::assistant(
            "second".to_string(),
            Some("2024-01-01T12:30:00+00:00".to_string()),
        ));

        let last_ts = transcript.last_message_timestamp_unix();
        assert!(last_ts.is_some());
        // 2024-01-01T12:30:00Z is 1704112200
        assert_eq!(last_ts.unwrap(), 1704112200);
    }

    #[test]
    fn test_ai_transcript_timestamp_unix_no_messages() {
        let transcript = AiTranscript::new();
        assert_eq!(transcript.first_message_timestamp_unix(), None);
        assert_eq!(transcript.last_message_timestamp_unix(), None);
    }

    #[test]
    fn test_ai_transcript_timestamp_unix_no_timestamps() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("text".to_string(), None));

        assert_eq!(transcript.first_message_timestamp_unix(), None);
        assert_eq!(transcript.last_message_timestamp_unix(), None);
    }

    #[test]
    fn test_ai_transcript_timestamp_unix_invalid_format() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user(
            "text".to_string(),
            Some("invalid-timestamp".to_string()),
        ));

        assert_eq!(transcript.first_message_timestamp_unix(), None);
        assert_eq!(transcript.last_message_timestamp_unix(), None);
    }

    #[test]
    fn test_ai_transcript_default() {
        let transcript = AiTranscript::default();
        assert!(transcript.messages.is_empty());
    }

    #[test]
    fn test_message_serialization() {
        let msg = Message::user(
            "Hello".to_string(),
            Some("2024-01-01T00:00:00Z".to_string()),
        );
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"user\""));
        assert!(json.contains("\"text\":\"Hello\""));
        assert!(json.contains("\"timestamp\":\"2024-01-01T00:00:00Z\""));
    }

    #[test]
    fn test_message_deserialization() {
        let json = r#"{"type":"user","text":"Hello","timestamp":"2024-01-01T00:00:00Z"}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        match msg {
            Message::User { text, timestamp } => {
                assert_eq!(text, "Hello");
                assert_eq!(timestamp, Some("2024-01-01T00:00:00Z".to_string()));
            }
            _ => panic!("Expected User message"),
        }
    }

    #[test]
    fn test_message_skip_none_timestamp() {
        let msg = Message::user("Hello".to_string(), None);
        let json = serde_json::to_string(&msg).unwrap();
        // timestamp should be omitted when None
        assert!(!json.contains("timestamp"));
    }

    #[test]
    fn test_ai_transcript_serialization() {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::user("Hello".to_string(), None));
        transcript.add_message(Message::assistant("Hi".to_string(), None));

        let json = serde_json::to_string(&transcript).unwrap();
        assert!(json.contains("\"messages\""));
        assert!(json.contains("\"type\":\"user\""));
        assert!(json.contains("\"type\":\"assistant\""));
    }

    #[test]
    fn test_ai_transcript_deserialization() {
        let json =
            r#"{"messages":[{"type":"user","text":"Hello"},{"type":"assistant","text":"Hi"}]}"#;
        let transcript: AiTranscript = serde_json::from_str(json).unwrap();
        assert_eq!(transcript.messages.len(), 2);
    }

    #[test]
    fn test_message_equality() {
        let msg1 = Message::user("text".to_string(), Some("ts".to_string()));
        let msg2 = Message::user("text".to_string(), Some("ts".to_string()));
        let msg3 = Message::user("different".to_string(), Some("ts".to_string()));

        assert_eq!(msg1, msg2);
        assert_ne!(msg1, msg3);
    }

    #[test]
    fn test_ai_transcript_equality() {
        let mut t1 = AiTranscript::new();
        t1.add_message(Message::user("msg".to_string(), None));

        let mut t2 = AiTranscript::new();
        t2.add_message(Message::user("msg".to_string(), None));

        let mut t3 = AiTranscript::new();
        t3.add_message(Message::user("different".to_string(), None));

        assert_eq!(t1, t2);
        assert_ne!(t1, t3);
    }
}
