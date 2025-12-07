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

    /// Create a tool use message
    pub fn tool_use(name: String, input: serde_json::Value) -> Self {
        Message::ToolUse {
            name,
            input,
            timestamp: None,
        }
    }

    /// Get the text content if this is a user or assistant message
    #[allow(dead_code)]
    pub fn text(&self) -> Option<&String> {
        match self {
            Message::User { text, .. } | Message::Assistant { text, .. } => Some(text),
            Message::ToolUse { .. } => None,
        }
    }

    /// Check if this is a tool use message
    #[allow(dead_code)]
    pub fn is_tool_use(&self) -> bool {
        matches!(self, Message::ToolUse { .. })
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
}

impl Default for AiTranscript {
    fn default() -> Self {
        Self::new()
    }
}
