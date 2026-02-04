use crate::authorship::attribution_tracker::{Attribution, LineAttribution};
use crate::authorship::authorship_log_serialization::GIT_AI_VERSION;
use crate::authorship::transcript::AiTranscript;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub const CHECKPOINT_API_VERSION: &str = "checkpoint/1.0.0";

/// Represents a working log entry for a specific file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingLogEntry {
    /// The file path relative to the repository root
    pub file: String,
    /// SHA256 hash of the file content at this checkpoint
    #[serde(default)]
    pub blob_sha: String,
    #[serde(default)]
    pub attributions: Vec<Attribution>,
    #[serde(default)]
    pub line_attributions: Vec<LineAttribution>,
}

impl WorkingLogEntry {
    /// Create a new working log entry
    pub fn new(
        file: String,
        blob_sha: String,
        attributions: Vec<Attribution>,
        line_attributions: Vec<LineAttribution>,
    ) -> Self {
        Self {
            file,
            blob_sha,
            attributions,
            line_attributions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentId {
    pub tool: String, // e.g., "cursor", "windsurf"
    pub id: String,   // id in their domain
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointKind {
    Human,
    AiAgent,
    AiTab,
}

impl fmt::Display for CheckpointKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_str())
    }
}

impl CheckpointKind {
    #[allow(dead_code)]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "human" => CheckpointKind::Human,
            "ai_agent" => CheckpointKind::AiAgent,
            "ai_tab" => CheckpointKind::AiTab,
            _ => panic!("Invalid checkpoint kind: {}", s),
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn to_str(&self) -> String {
        match self {
            CheckpointKind::Human => "human".to_string(),
            CheckpointKind::AiAgent => "ai_agent".to_string(),
            CheckpointKind::AiTab => "ai_tab".to_string(),
        }
    }

    /// Default value to prevent crashes on old versions
    pub fn serde_default() -> Self {
        CheckpointKind::Human
    }
}

/// Line-level statistics tracked per checkpoint kind
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CheckpointLineStats {
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    #[serde(default)]
    pub additions_sloc: u32,
    #[serde(default)]
    pub deletions_sloc: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    #[serde(default = "CheckpointKind::serde_default")]
    pub kind: CheckpointKind,
    pub diff: String,
    pub author: String,
    pub entries: Vec<WorkingLogEntry>,
    pub timestamp: u64,
    pub transcript: Option<AiTranscript>,
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub agent_metadata: Option<HashMap<String, String>>,
    #[serde(default)]
    pub line_stats: CheckpointLineStats,
    #[serde(default)]
    pub api_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ai_version: Option<String>,
}

impl Checkpoint {
    pub fn new(
        kind: CheckpointKind,
        diff: String,
        author: String,
        entries: Vec<WorkingLogEntry>,
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            kind,
            diff,
            author,
            entries,
            timestamp,
            transcript: None,
            agent_id: None,
            agent_metadata: None,
            line_stats: CheckpointLineStats::default(),
            api_version: CHECKPOINT_API_VERSION.to_string(),
            git_ai_version: Some(GIT_AI_VERSION.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::transcript::Message;

    #[test]
    fn test_checkpoint_serialization() {
        let entry = WorkingLogEntry::new(
            "src/xyz.rs".to_string(),
            "abc123def456".to_string(),
            Vec::new(),
            Vec::new(),
        );
        let checkpoint = Checkpoint::new(
            CheckpointKind::AiAgent,
            "".to_string(),
            "claude".to_string(),
            vec![entry],
        );

        // Verify timestamp is set (should be recent)
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        assert!(checkpoint.timestamp > 0);
        assert!(checkpoint.timestamp <= current_time);
        assert!(checkpoint.transcript.is_none());
        assert!(checkpoint.agent_id.is_none());

        let json = serde_json::to_string_pretty(&checkpoint).unwrap();
        let deserialized: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.diff, "");
        assert_eq!(deserialized.entries.len(), 1);
        assert_eq!(deserialized.entries[0].file, "src/xyz.rs");
        assert_eq!(deserialized.entries[0].blob_sha, "abc123def456");
        assert_eq!(deserialized.timestamp, checkpoint.timestamp);
        assert!(deserialized.transcript.is_none());
        assert!(deserialized.agent_id.is_none());
    }

    #[test]
    fn test_log_array_serialization() {
        let entry1 = WorkingLogEntry::new(
            "src/xyz.rs".to_string(),
            "sha1".to_string(),
            Vec::new(),
            Vec::new(),
        );
        let checkpoint1 = Checkpoint::new(
            CheckpointKind::AiAgent,
            "".to_string(),
            "claude".to_string(),
            vec![entry1],
        );

        let entry2 = WorkingLogEntry::new(
            "src/xyz.rs".to_string(),
            "sha2".to_string(),
            Vec::new(),
            Vec::new(),
        );
        let checkpoint2 = Checkpoint::new(
            CheckpointKind::AiAgent,
            "/refs/ai/working/xyz.patch".to_string(),
            "user".to_string(),
            vec![entry2],
        );

        // Verify timestamps are set and checkpoint2 is newer than checkpoint1
        assert!(checkpoint1.timestamp > 0);
        assert!(checkpoint2.timestamp > 0);
        assert!(checkpoint2.timestamp >= checkpoint1.timestamp);

        let log = vec![checkpoint1, checkpoint2];
        let json = serde_json::to_string_pretty(&log).unwrap();
        // println!("Working log array JSON:\n{}", json);
        let deserialized: Vec<Checkpoint> = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized[0].diff, "");
        assert_eq!(deserialized[1].diff, "/refs/ai/working/xyz.patch");
        assert_eq!(deserialized[1].author, "user");
    }

    #[test]
    fn test_checkpoint_with_transcript() {
        let entry = WorkingLogEntry::new(
            "src/xyz.rs".to_string(),
            "test_sha".to_string(),
            Vec::new(),
            Vec::new(),
        );

        let user_message = Message::user(
            "Please add error handling to this function".to_string(),
            None,
        );
        let assistant_message =
            Message::assistant("I'll add error handling to the function.".to_string(), None);

        let mut transcript = AiTranscript::new();
        transcript.add_message(user_message);
        transcript.add_message(assistant_message);

        let agent_id = AgentId {
            tool: "cursor".to_string(),
            model: "gpt-4o".to_string(),
            id: "session-abc123".to_string(),
        };

        let mut checkpoint = Checkpoint::new(
            CheckpointKind::AiAgent,
            "".to_string(),
            "claude".to_string(),
            vec![entry],
        );
        checkpoint.transcript = Some(transcript);
        checkpoint.agent_id = Some(agent_id);

        assert!(checkpoint.transcript.is_some());
        assert!(checkpoint.agent_id.is_some());

        let transcript_data = checkpoint.transcript.as_ref().unwrap();
        assert_eq!(transcript_data.messages().len(), 2);

        // Check first message (user)
        match &transcript_data.messages()[0] {
            Message::User { text, .. } => {
                assert_eq!(text, "Please add error handling to this function");
            }
            _ => panic!("Expected user message"),
        }

        // Check second message (assistant)
        match &transcript_data.messages()[1] {
            Message::Assistant { text, .. } => {
                assert_eq!(text, "I'll add error handling to the function.");
            }
            _ => panic!("Expected assistant message"),
        }

        let agent_data = checkpoint.agent_id.as_ref().unwrap();
        assert_eq!(agent_data.tool, "cursor");
        assert_eq!(agent_data.id, "session-abc123");

        let json = serde_json::to_string_pretty(&checkpoint).unwrap();
        let deserialized: Checkpoint = serde_json::from_str(&json).unwrap();
        assert!(deserialized.transcript.is_some());
        assert!(deserialized.agent_id.is_some());

        let deserialized_transcript = deserialized.transcript.as_ref().unwrap();
        assert_eq!(deserialized_transcript.messages().len(), 2);

        let deserialized_agent = deserialized.agent_id.as_ref().unwrap();
        assert_eq!(deserialized_agent.tool, "cursor");
        assert_eq!(deserialized_agent.id, "session-abc123");
    }
}
