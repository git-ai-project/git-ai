use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metadata attached to a generated synopsis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynopsisMetadata {
    pub commit_sha: String,
    pub date: DateTime<Utc>,
    pub author: String,
    pub model: String,
    pub version: u32,
    pub word_count: usize,
    pub input_tokens_estimate: usize,
    pub conversation_source: Option<String>,
    pub conversation_window_secs: Option<u64>,
    pub files_changed: usize,
}

/// A generated AI synopsis for a commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Synopsis {
    pub metadata: SynopsisMetadata,
    /// Markdown body of the synopsis.
    pub content: String,
}

/// A single exchange in a conversation log.
#[derive(Debug, Clone)]
pub struct ConversationExchange {
    pub speaker: Speaker,
    pub text: String,
    pub timestamp: Option<DateTime<Utc>>,
}

/// Who spoke in a conversation exchange.
#[derive(Debug, Clone, PartialEq)]
pub enum Speaker {
    User,
    Assistant,
    ToolUse(String),
}

/// A parsed conversation log from an AI coding session.
#[derive(Debug, Clone)]
pub struct ConversationLog {
    /// The kind of source, e.g. `"claude-code"`.
    pub source_kind: String,
    pub exchanges: Vec<ConversationExchange>,
    pub source_path: String,
}

/// The diff between two commits, pre-computed for injection into prompts.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DiffBundle {
    pub stat_summary: String,
    pub unified_diff: String,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// All inputs required to generate a synopsis.
#[derive(Debug, Clone)]
pub struct SynopsisInput {
    pub conversation: Option<ConversationLog>,
    pub diff: DiffBundle,
    pub commit_message: String,
    pub commit_sha: String,
    pub author: String,
}
