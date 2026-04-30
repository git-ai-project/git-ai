//! Event-specific value structs for metrics.

use super::pos_encoded::{
    PosEncoded, PosField, f64_to_json, sparse_get_f64, sparse_get_string, sparse_get_u32,
    sparse_get_u64, sparse_get_vec_string, sparse_get_vec_u32, sparse_set, string_to_json,
    u32_to_json, u64_to_json, vec_string_to_json, vec_u32_to_json,
};
use super::types::{EventValues, MetricEventId, SparseArray};

/// Value positions for "committed" event.
pub mod committed_pos {
    // Scalar fields
    pub const HUMAN_ADDITIONS: usize = 0;
    pub const GIT_DIFF_DELETED_LINES: usize = 1;
    pub const GIT_DIFF_ADDED_LINES: usize = 2;

    // Array fields (parallel arrays, index 0 = "all" aggregate, index 1+ = per tool/model)
    pub const TOOL_MODEL_PAIRS: usize = 3;
    pub const MIXED_ADDITIONS: usize = 4;
    pub const AI_ADDITIONS: usize = 5;
    pub const AI_ACCEPTED: usize = 6;
    pub const TOTAL_AI_ADDITIONS: usize = 7;
    pub const TOTAL_AI_DELETIONS: usize = 8;
    // Position 9 was time_waiting_for_ai (removed)

    // New scalar fields
    pub const FIRST_CHECKPOINT_TS: usize = 10; // u64 (null if no checkpoints)
    pub const COMMIT_SUBJECT: usize = 11; // String
    pub const COMMIT_BODY: usize = 12; // String (null if empty)
}

/// Values for Event ID 1: committed
///
/// Recorded when AI-assisted code is committed.
///
/// **Scalar fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | human_additions | u32 |
/// | 1 | git_diff_deleted_lines | u32 |
/// | 2 | git_diff_added_lines | u32 |
///
/// **Array fields (parallel arrays, index 0 = "all" for aggregate, index 1+ = per tool/model):**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 3 | tool_model_pairs | `Vec<String>` |
/// | 4 | (removed) | - |
/// | 5 | ai_additions | `Vec<u32>` |
/// | 6 | ai_accepted | `Vec<u32>` |
/// | 7 | (removed) | - |
/// | 8 | (removed) | - |
/// | 9 | (removed) | - |
/// | 10 | first_checkpoint_ts | u64 |
/// | 11 | commit_subject | String |
/// | 12 | commit_body | String |
#[derive(Debug, Clone, Default)]
pub struct CommittedValues {
    // Scalar fields
    pub human_additions: PosField<u32>,
    pub git_diff_deleted_lines: PosField<u32>,
    pub git_diff_added_lines: PosField<u32>,

    // Array fields (parallel arrays)
    pub tool_model_pairs: PosField<Vec<String>>,
    pub ai_additions: PosField<Vec<u32>>,
    pub ai_accepted: PosField<Vec<u32>>,

    // New scalar fields
    pub first_checkpoint_ts: PosField<u64>,
    pub commit_subject: PosField<String>,
    pub commit_body: PosField<String>,
}

impl CommittedValues {
    pub fn new() -> Self {
        Self::default()
    }

    // Builder methods for scalar fields

    pub fn human_additions(mut self, value: u32) -> Self {
        self.human_additions = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn human_additions_null(mut self) -> Self {
        self.human_additions = Some(None);
        self
    }

    pub fn git_diff_deleted_lines(mut self, value: u32) -> Self {
        self.git_diff_deleted_lines = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn git_diff_deleted_lines_null(mut self) -> Self {
        self.git_diff_deleted_lines = Some(None);
        self
    }

    pub fn git_diff_added_lines(mut self, value: u32) -> Self {
        self.git_diff_added_lines = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn git_diff_added_lines_null(mut self) -> Self {
        self.git_diff_added_lines = Some(None);
        self
    }

    // Builder methods for array fields

    pub fn tool_model_pairs(mut self, value: Vec<String>) -> Self {
        self.tool_model_pairs = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn tool_model_pairs_null(mut self) -> Self {
        self.tool_model_pairs = Some(None);
        self
    }

    pub fn ai_additions(mut self, value: Vec<u32>) -> Self {
        self.ai_additions = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn ai_additions_null(mut self) -> Self {
        self.ai_additions = Some(None);
        self
    }

    pub fn ai_accepted(mut self, value: Vec<u32>) -> Self {
        self.ai_accepted = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn ai_accepted_null(mut self) -> Self {
        self.ai_accepted = Some(None);
        self
    }

    // Builder methods for new scalar fields

    pub fn first_checkpoint_ts(mut self, value: u64) -> Self {
        self.first_checkpoint_ts = Some(Some(value));
        self
    }

    pub fn first_checkpoint_ts_null(mut self) -> Self {
        self.first_checkpoint_ts = Some(None);
        self
    }

    pub fn commit_subject(mut self, value: impl Into<String>) -> Self {
        self.commit_subject = Some(Some(value.into()));
        self
    }

    pub fn commit_subject_null(mut self) -> Self {
        self.commit_subject = Some(None);
        self
    }

    pub fn commit_body(mut self, value: impl Into<String>) -> Self {
        self.commit_body = Some(Some(value.into()));
        self
    }

    pub fn commit_body_null(mut self) -> Self {
        self.commit_body = Some(None);
        self
    }
}

impl PosEncoded for CommittedValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();

        // Scalar fields
        sparse_set(
            &mut map,
            committed_pos::HUMAN_ADDITIONS,
            u32_to_json(&self.human_additions),
        );
        sparse_set(
            &mut map,
            committed_pos::GIT_DIFF_DELETED_LINES,
            u32_to_json(&self.git_diff_deleted_lines),
        );
        sparse_set(
            &mut map,
            committed_pos::GIT_DIFF_ADDED_LINES,
            u32_to_json(&self.git_diff_added_lines),
        );

        // Array fields
        sparse_set(
            &mut map,
            committed_pos::TOOL_MODEL_PAIRS,
            vec_string_to_json(&self.tool_model_pairs),
        );
        sparse_set(
            &mut map,
            committed_pos::AI_ADDITIONS,
            vec_u32_to_json(&self.ai_additions),
        );
        sparse_set(
            &mut map,
            committed_pos::AI_ACCEPTED,
            vec_u32_to_json(&self.ai_accepted),
        );

        // New scalar fields
        sparse_set(
            &mut map,
            committed_pos::FIRST_CHECKPOINT_TS,
            u64_to_json(&self.first_checkpoint_ts),
        );
        sparse_set(
            &mut map,
            committed_pos::COMMIT_SUBJECT,
            string_to_json(&self.commit_subject),
        );
        sparse_set(
            &mut map,
            committed_pos::COMMIT_BODY,
            string_to_json(&self.commit_body),
        );

        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            // Scalar fields
            human_additions: sparse_get_u32(arr, committed_pos::HUMAN_ADDITIONS),
            git_diff_deleted_lines: sparse_get_u32(arr, committed_pos::GIT_DIFF_DELETED_LINES),
            git_diff_added_lines: sparse_get_u32(arr, committed_pos::GIT_DIFF_ADDED_LINES),

            // Array fields
            tool_model_pairs: sparse_get_vec_string(arr, committed_pos::TOOL_MODEL_PAIRS),
            ai_additions: sparse_get_vec_u32(arr, committed_pos::AI_ADDITIONS),
            ai_accepted: sparse_get_vec_u32(arr, committed_pos::AI_ACCEPTED),

            // New scalar fields
            first_checkpoint_ts: sparse_get_u64(arr, committed_pos::FIRST_CHECKPOINT_TS),
            commit_subject: sparse_get_string(arr, committed_pos::COMMIT_SUBJECT),
            commit_body: sparse_get_string(arr, committed_pos::COMMIT_BODY),
        }
    }
}

impl EventValues for CommittedValues {
    fn event_id() -> MetricEventId {
        MetricEventId::Committed
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Values for Event ID 2: agent_usage
///
/// Recorded on every AI checkpoint to track agent usage.
/// Uses attributes (prompt_id, tool, model) rather than event-specific values.
#[derive(Debug, Clone, Default)]
pub struct AgentUsageValues {}

impl AgentUsageValues {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PosEncoded for AgentUsageValues {
    fn to_sparse(&self) -> SparseArray {
        SparseArray::new()
    }

    fn from_sparse(_arr: &SparseArray) -> Self {
        Self::default()
    }
}

impl EventValues for AgentUsageValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentUsage
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "install_hooks" event.
/// One event per tool attempted during install-hooks.
pub mod install_hooks_pos {
    pub const TOOL_ID: usize = 0; // String - tool id (e.g., "cursor", "fork")
    pub const STATUS: usize = 1; // String - "not_found", "installed", "already_installed", "failed"
    pub const MESSAGE: usize = 2; // Option<String> - error message or warnings
}

/// Values for Event ID 3: install_hooks
///
/// Recorded for each tool during git-ai install-hooks command.
/// One event per tool attempted.
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | tool_id | String |
/// | 1 | status | String |
/// | 2 | message | `Option<String>` |
#[derive(Debug, Clone, Default)]
pub struct InstallHooksValues {
    pub tool_id: PosField<String>,
    pub status: PosField<String>,
    pub message: PosField<String>,
}

impl InstallHooksValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tool_id(mut self, value: String) -> Self {
        self.tool_id = Some(Some(value));
        self
    }

    pub fn status(mut self, value: String) -> Self {
        self.status = Some(Some(value));
        self
    }

    pub fn message(mut self, value: String) -> Self {
        self.message = Some(Some(value));
        self
    }

    pub fn message_null(mut self) -> Self {
        self.message = Some(None);
        self
    }
}

impl PosEncoded for InstallHooksValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();

        sparse_set(
            &mut map,
            install_hooks_pos::TOOL_ID,
            string_to_json(&self.tool_id),
        );
        sparse_set(
            &mut map,
            install_hooks_pos::STATUS,
            string_to_json(&self.status),
        );
        sparse_set(
            &mut map,
            install_hooks_pos::MESSAGE,
            string_to_json(&self.message),
        );

        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            tool_id: sparse_get_string(arr, install_hooks_pos::TOOL_ID),
            status: sparse_get_string(arr, install_hooks_pos::STATUS),
            message: sparse_get_string(arr, install_hooks_pos::MESSAGE),
        }
    }
}

impl EventValues for InstallHooksValues {
    fn event_id() -> MetricEventId {
        MetricEventId::InstallHooks
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "checkpoint" event.
/// One event per file in the checkpoint.
pub mod checkpoint_pos {
    pub const CHECKPOINT_TS: usize = 0; // u64 - checkpoint timestamp
    pub const KIND: usize = 1; // String ("human", "ai_agent", "ai_tab")
    pub const FILE_PATH: usize = 2; // String - full relative file path
    pub const LINES_ADDED: usize = 3; // u32 - for this file
    pub const LINES_DELETED: usize = 4; // u32 - for this file
    pub const LINES_ADDED_SLOC: usize = 5; // u32 - for this file
    pub const LINES_DELETED_SLOC: usize = 6; // u32 - for this file
    pub const TOOL_USE_ID: usize = 7; // String - nullable
}

/// Values for Event ID 4: checkpoint
///
/// Recorded for each file in a checkpoint.
/// Uses EventAttributes for standard metadata (repo_url, author, tool, model, etc.)
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | checkpoint_ts | u64 |
/// | 1 | kind | String |
/// | 2 | file_path | String |
/// | 3 | lines_added | u32 |
/// | 4 | lines_deleted | u32 |
/// | 5 | lines_added_sloc | u32 |
/// | 6 | lines_deleted_sloc | u32 |
/// | 7 | external_tool_use_id | String (nullable) |
#[derive(Debug, Clone, Default)]
pub struct CheckpointValues {
    pub checkpoint_ts: PosField<u64>,
    pub kind: PosField<String>,
    pub file_path: PosField<String>,
    pub lines_added: PosField<u32>,
    pub lines_deleted: PosField<u32>,
    pub lines_added_sloc: PosField<u32>,
    pub lines_deleted_sloc: PosField<u32>,
    pub external_tool_use_id: PosField<String>,
}

impl CheckpointValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn checkpoint_ts(mut self, value: u64) -> Self {
        self.checkpoint_ts = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn checkpoint_ts_null(mut self) -> Self {
        self.checkpoint_ts = Some(None);
        self
    }

    pub fn kind(mut self, value: impl Into<String>) -> Self {
        self.kind = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn kind_null(mut self) -> Self {
        self.kind = Some(None);
        self
    }

    pub fn file_path(mut self, value: impl Into<String>) -> Self {
        self.file_path = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn file_path_null(mut self) -> Self {
        self.file_path = Some(None);
        self
    }

    pub fn lines_added(mut self, value: u32) -> Self {
        self.lines_added = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn lines_added_null(mut self) -> Self {
        self.lines_added = Some(None);
        self
    }

    pub fn lines_deleted(mut self, value: u32) -> Self {
        self.lines_deleted = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn lines_deleted_null(mut self) -> Self {
        self.lines_deleted = Some(None);
        self
    }

    pub fn lines_added_sloc(mut self, value: u32) -> Self {
        self.lines_added_sloc = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn lines_added_sloc_null(mut self) -> Self {
        self.lines_added_sloc = Some(None);
        self
    }

    pub fn lines_deleted_sloc(mut self, value: u32) -> Self {
        self.lines_deleted_sloc = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn lines_deleted_sloc_null(mut self) -> Self {
        self.lines_deleted_sloc = Some(None);
        self
    }

    pub fn external_tool_use_id(mut self, value: impl Into<String>) -> Self {
        self.external_tool_use_id = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn external_tool_use_id_null(mut self) -> Self {
        self.external_tool_use_id = Some(None);
        self
    }
}

impl PosEncoded for CheckpointValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();

        sparse_set(
            &mut map,
            checkpoint_pos::CHECKPOINT_TS,
            u64_to_json(&self.checkpoint_ts),
        );
        sparse_set(&mut map, checkpoint_pos::KIND, string_to_json(&self.kind));
        sparse_set(
            &mut map,
            checkpoint_pos::FILE_PATH,
            string_to_json(&self.file_path),
        );
        sparse_set(
            &mut map,
            checkpoint_pos::LINES_ADDED,
            u32_to_json(&self.lines_added),
        );
        sparse_set(
            &mut map,
            checkpoint_pos::LINES_DELETED,
            u32_to_json(&self.lines_deleted),
        );
        sparse_set(
            &mut map,
            checkpoint_pos::LINES_ADDED_SLOC,
            u32_to_json(&self.lines_added_sloc),
        );
        sparse_set(
            &mut map,
            checkpoint_pos::LINES_DELETED_SLOC,
            u32_to_json(&self.lines_deleted_sloc),
        );
        sparse_set(
            &mut map,
            checkpoint_pos::TOOL_USE_ID,
            string_to_json(&self.external_tool_use_id),
        );

        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            checkpoint_ts: sparse_get_u64(arr, checkpoint_pos::CHECKPOINT_TS),
            kind: sparse_get_string(arr, checkpoint_pos::KIND),
            file_path: sparse_get_string(arr, checkpoint_pos::FILE_PATH),
            lines_added: sparse_get_u32(arr, checkpoint_pos::LINES_ADDED),
            lines_deleted: sparse_get_u32(arr, checkpoint_pos::LINES_DELETED),
            lines_added_sloc: sparse_get_u32(arr, checkpoint_pos::LINES_ADDED_SLOC),
            lines_deleted_sloc: sparse_get_u32(arr, checkpoint_pos::LINES_DELETED_SLOC),
            external_tool_use_id: sparse_get_string(arr, checkpoint_pos::TOOL_USE_ID),
        }
    }
}

impl EventValues for CheckpointValues {
    fn event_id() -> MetricEventId {
        MetricEventId::Checkpoint
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn test_committed_values_builder() {
        let values = CommittedValues::new()
            .human_additions(50)
            .git_diff_deleted_lines(20)
            .git_diff_added_lines(150)
            .tool_model_pairs(vec!["all".to_string(), "claude-code:claude-3".to_string()])
            .ai_additions(vec![100, 70])
            .ai_accepted(vec![80, 55]);

        assert_eq!(values.human_additions, Some(Some(50)));
        assert_eq!(
            values.tool_model_pairs,
            Some(Some(vec![
                "all".to_string(),
                "claude-code:claude-3".to_string()
            ]))
        );
        assert_eq!(values.ai_additions, Some(Some(vec![100, 70])));
    }

    #[test]
    fn test_committed_values_to_sparse() {
        use super::PosEncoded;

        let values = CommittedValues::new()
            .human_additions(50)
            .git_diff_deleted_lines(20)
            .git_diff_added_lines(150)
            .tool_model_pairs(vec!["all".to_string(), "cursor:gpt-4".to_string()])
            .ai_additions(vec![100, 30]);

        let sparse = PosEncoded::to_sparse(&values);

        assert_eq!(sparse.get("0"), Some(&Value::Number(50.into())));
        assert_eq!(sparse.get("1"), Some(&Value::Number(20.into())));
        assert_eq!(sparse.get("2"), Some(&Value::Number(150.into())));
        assert_eq!(
            sparse.get("3"),
            Some(&Value::Array(vec![
                Value::String("all".to_string()),
                Value::String("cursor:gpt-4".to_string())
            ]))
        );
        assert_eq!(
            sparse.get("5"),
            Some(&Value::Array(vec![
                Value::Number(100.into()),
                Value::Number(30.into())
            ]))
        );
    }

    #[test]
    fn test_committed_values_from_sparse() {
        use super::PosEncoded;

        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::Number(75.into()));
        sparse.insert(
            "3".to_string(),
            Value::Array(vec![
                Value::String("all".to_string()),
                Value::String("copilot:gpt-4".to_string()),
            ]),
        );
        sparse.insert(
            "5".to_string(),
            Value::Array(vec![Value::Number(200.into()), Value::Number(100.into())]),
        );

        let values = <CommittedValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(values.human_additions, Some(Some(75)));
        assert_eq!(
            values.tool_model_pairs,
            Some(Some(vec!["all".to_string(), "copilot:gpt-4".to_string()]))
        );
        assert_eq!(values.ai_additions, Some(Some(vec![200, 100])));
        assert_eq!(values.git_diff_deleted_lines, None); // not set
    }

    #[test]
    fn test_committed_values_event_id() {
        assert_eq!(CommittedValues::event_id(), MetricEventId::Committed);
        assert_eq!(CommittedValues::event_id() as u16, 1);
    }

    #[test]
    fn test_committed_values_null_fields() {
        let values = CommittedValues::new()
            .human_additions_null()
            .git_diff_deleted_lines_null()
            .tool_model_pairs_null();

        assert_eq!(values.human_additions, Some(None));
        assert_eq!(values.git_diff_deleted_lines, Some(None));
        assert_eq!(values.tool_model_pairs, Some(None));
    }

    #[test]
    fn test_committed_values_with_commit_info() {
        let values = CommittedValues::new()
            .human_additions(10)
            .first_checkpoint_ts(1704067200)
            .commit_subject("Initial commit")
            .commit_body("This is the commit body\n\nWith multiple lines");

        assert_eq!(values.first_checkpoint_ts, Some(Some(1704067200)));
        assert_eq!(
            values.commit_subject,
            Some(Some("Initial commit".to_string()))
        );
        assert_eq!(
            values.commit_body,
            Some(Some(
                "This is the commit body\n\nWith multiple lines".to_string()
            ))
        );
    }

    #[test]
    fn test_committed_values_roundtrip_with_new_fields() {
        use super::PosEncoded;

        let original = CommittedValues::new()
            .human_additions(25)
            .first_checkpoint_ts(1700000000)
            .commit_subject("Test commit")
            .commit_body_null();

        let sparse = PosEncoded::to_sparse(&original);
        let restored = <CommittedValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.human_additions, Some(Some(25)));
        assert_eq!(restored.first_checkpoint_ts, Some(Some(1700000000)));
        assert_eq!(
            restored.commit_subject,
            Some(Some("Test commit".to_string()))
        );
        assert_eq!(restored.commit_body, Some(None));
    }

    #[test]
    fn test_agent_usage_values() {
        let values = AgentUsageValues::new();
        assert_eq!(AgentUsageValues::event_id(), MetricEventId::AgentUsage);
        assert_eq!(AgentUsageValues::event_id() as u16, 2);

        // Should produce empty sparse array
        let sparse = PosEncoded::to_sparse(&values);
        assert!(sparse.is_empty());
    }

    #[test]
    fn test_agent_usage_values_roundtrip() {
        use super::PosEncoded;

        let original = AgentUsageValues::new();
        let sparse = PosEncoded::to_sparse(&original);
        let restored = <AgentUsageValues as PosEncoded>::from_sparse(&sparse);

        // Both should be empty
        assert!(PosEncoded::to_sparse(&restored).is_empty());
    }

    #[test]
    fn test_install_hooks_values_builder() {
        let values = InstallHooksValues::new()
            .tool_id("cursor".to_string())
            .status("installed".to_string())
            .message("Successfully installed".to_string());

        assert_eq!(values.tool_id, Some(Some("cursor".to_string())));
        assert_eq!(values.status, Some(Some("installed".to_string())));
        assert_eq!(
            values.message,
            Some(Some("Successfully installed".to_string()))
        );
    }

    #[test]
    fn test_install_hooks_values_with_null_message() {
        let values = InstallHooksValues::new()
            .tool_id("fork".to_string())
            .status("not_found".to_string())
            .message_null();

        assert_eq!(values.message, Some(None));
    }

    #[test]
    fn test_install_hooks_values_to_sparse() {
        use super::PosEncoded;

        let values = InstallHooksValues::new()
            .tool_id("copilot".to_string())
            .status("failed".to_string())
            .message("Error: permission denied".to_string());

        let sparse = PosEncoded::to_sparse(&values);

        assert_eq!(sparse.get("0"), Some(&Value::String("copilot".to_string())));
        assert_eq!(sparse.get("1"), Some(&Value::String("failed".to_string())));
        assert_eq!(
            sparse.get("2"),
            Some(&Value::String("Error: permission denied".to_string()))
        );
    }

    #[test]
    fn test_install_hooks_values_from_sparse() {
        use super::PosEncoded;

        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("windsurf".to_string()));
        sparse.insert(
            "1".to_string(),
            Value::String("already_installed".to_string()),
        );
        sparse.insert("2".to_string(), Value::Null);

        let values = <InstallHooksValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(values.tool_id, Some(Some("windsurf".to_string())));
        assert_eq!(values.status, Some(Some("already_installed".to_string())));
        assert_eq!(values.message, Some(None));
    }

    #[test]
    fn test_install_hooks_event_id() {
        assert_eq!(InstallHooksValues::event_id(), MetricEventId::InstallHooks);
        assert_eq!(InstallHooksValues::event_id() as u16, 3);
    }

    #[test]
    fn test_checkpoint_values_builder() {
        let values = CheckpointValues::new()
            .checkpoint_ts(1704067200)
            .kind("ai_agent")
            .file_path("src/main.rs")
            .lines_added(50)
            .lines_deleted(10)
            .lines_added_sloc(45)
            .lines_deleted_sloc(8);

        assert_eq!(values.checkpoint_ts, Some(Some(1704067200)));
        assert_eq!(values.kind, Some(Some("ai_agent".to_string())));
        assert_eq!(values.file_path, Some(Some("src/main.rs".to_string())));
        assert_eq!(values.lines_added, Some(Some(50)));
        assert_eq!(values.lines_deleted, Some(Some(10)));
        assert_eq!(values.lines_added_sloc, Some(Some(45)));
        assert_eq!(values.lines_deleted_sloc, Some(Some(8)));
    }

    #[test]
    fn test_checkpoint_values_with_nulls() {
        let values = CheckpointValues::new()
            .checkpoint_ts_null()
            .kind_null()
            .file_path_null()
            .lines_added_null();

        assert_eq!(values.checkpoint_ts, Some(None));
        assert_eq!(values.kind, Some(None));
        assert_eq!(values.file_path, Some(None));
        assert_eq!(values.lines_added, Some(None));
    }

    #[test]
    fn test_checkpoint_values_to_sparse() {
        use super::PosEncoded;

        let values = CheckpointValues::new()
            .checkpoint_ts(1700000000)
            .kind("human")
            .file_path("tests/test.rs")
            .lines_added(100)
            .lines_deleted(20);

        let sparse = PosEncoded::to_sparse(&values);

        assert_eq!(sparse.get("0"), Some(&Value::Number(1700000000.into())));
        assert_eq!(sparse.get("1"), Some(&Value::String("human".to_string())));
        assert_eq!(
            sparse.get("2"),
            Some(&Value::String("tests/test.rs".to_string()))
        );
        assert_eq!(sparse.get("3"), Some(&Value::Number(100.into())));
        assert_eq!(sparse.get("4"), Some(&Value::Number(20.into())));
    }

    #[test]
    fn test_checkpoint_values_from_sparse() {
        use super::PosEncoded;

        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::Number(1704067200.into()));
        sparse.insert("1".to_string(), Value::String("ai_tab".to_string()));
        sparse.insert("2".to_string(), Value::String("lib.rs".to_string()));
        sparse.insert("3".to_string(), Value::Number(75.into()));
        sparse.insert("4".to_string(), Value::Number(15.into()));
        sparse.insert("5".to_string(), Value::Number(70.into()));
        sparse.insert("6".to_string(), Value::Number(12.into()));

        let values = <CheckpointValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(values.checkpoint_ts, Some(Some(1704067200)));
        assert_eq!(values.kind, Some(Some("ai_tab".to_string())));
        assert_eq!(values.file_path, Some(Some("lib.rs".to_string())));
        assert_eq!(values.lines_added, Some(Some(75)));
        assert_eq!(values.lines_deleted, Some(Some(15)));
        assert_eq!(values.lines_added_sloc, Some(Some(70)));
        assert_eq!(values.lines_deleted_sloc, Some(Some(12)));
    }

    #[test]
    fn test_checkpoint_event_id() {
        assert_eq!(CheckpointValues::event_id(), MetricEventId::Checkpoint);
        assert_eq!(CheckpointValues::event_id() as u16, 4);
    }

    #[test]
    fn test_committed_values_with_all_arrays() {
        let values = CommittedValues::new()
            .tool_model_pairs(vec!["all".to_string(), "cursor:gpt-4".to_string()])
            .ai_additions(vec![100, 50])
            .ai_accepted(vec![80, 40]);

        assert_eq!(
            values.tool_model_pairs,
            Some(Some(vec!["all".to_string(), "cursor:gpt-4".to_string()]))
        );
        assert_eq!(values.ai_additions, Some(Some(vec![100, 50])));
        assert_eq!(values.ai_accepted, Some(Some(vec![80, 40])));
    }

    #[test]
    fn test_committed_values_array_nulls() {
        let values = CommittedValues::new().ai_accepted_null();

        assert_eq!(values.ai_accepted, Some(None));
    }

    #[test]
    fn test_checkpoint_values_with_external_tool_use_id() {
        let values = CheckpointValues::new()
            .checkpoint_ts(1704067200)
            .kind("ai_agent")
            .file_path("src/main.rs")
            .lines_added(50)
            .external_tool_use_id("tool-use-123");

        assert_eq!(
            values.external_tool_use_id,
            Some(Some("tool-use-123".to_string()))
        );
    }

    #[test]
    fn test_checkpoint_values_external_tool_use_id_null() {
        let values = CheckpointValues::new()
            .checkpoint_ts(1704067200)
            .kind("human")
            .external_tool_use_id_null();

        assert_eq!(values.external_tool_use_id, Some(None));
    }

    #[test]
    fn test_checkpoint_values_to_sparse_with_external_tool_use_id() {
        use super::PosEncoded;

        let values = CheckpointValues::new()
            .checkpoint_ts(1700000000)
            .kind("ai_agent")
            .file_path("tests/test.rs")
            .lines_added(100)
            .external_tool_use_id("tool-xyz");

        let sparse = PosEncoded::to_sparse(&values);

        assert_eq!(sparse.get("0"), Some(&Value::Number(1700000000.into())));
        assert_eq!(
            sparse.get("1"),
            Some(&Value::String("ai_agent".to_string()))
        );
        assert_eq!(
            sparse.get("2"),
            Some(&Value::String("tests/test.rs".to_string()))
        );
        assert_eq!(sparse.get("3"), Some(&Value::Number(100.into())));
        assert_eq!(
            sparse.get("7"),
            Some(&Value::String("tool-xyz".to_string()))
        );
    }

    #[test]
    fn test_checkpoint_values_from_sparse_with_external_tool_use_id() {
        use super::PosEncoded;

        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::Number(1704067200.into()));
        sparse.insert("1".to_string(), Value::String("ai_tab".to_string()));
        sparse.insert("2".to_string(), Value::String("lib.rs".to_string()));
        sparse.insert("3".to_string(), Value::Number(75.into()));
        sparse.insert("7".to_string(), Value::String("tool-abc".to_string()));

        let values = <CheckpointValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(values.checkpoint_ts, Some(Some(1704067200)));
        assert_eq!(values.kind, Some(Some("ai_tab".to_string())));
        assert_eq!(values.file_path, Some(Some("lib.rs".to_string())));
        assert_eq!(values.lines_added, Some(Some(75)));
        assert_eq!(
            values.external_tool_use_id,
            Some(Some("tool-abc".to_string()))
        );
    }

    #[test]
    fn test_checkpoint_values_roundtrip_with_external_tool_use_id() {
        use super::PosEncoded;

        let original = CheckpointValues::new()
            .checkpoint_ts(1700000000)
            .kind("ai_agent")
            .file_path("src/lib.rs")
            .lines_added(50)
            .external_tool_use_id_null();

        let sparse = PosEncoded::to_sparse(&original);
        let restored = <CheckpointValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.checkpoint_ts, Some(Some(1700000000)));
        assert_eq!(restored.kind, Some(Some("ai_agent".to_string())));
        assert_eq!(restored.file_path, Some(Some("src/lib.rs".to_string())));
        assert_eq!(restored.lines_added, Some(Some(50)));
        assert_eq!(restored.external_tool_use_id, Some(None)); // explicitly null
    }

    #[test]
    fn test_checkpoint_values_external_tool_use_id_not_set() {
        use super::PosEncoded;

        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::Number(1700000000.into()));
        sparse.insert("1".to_string(), Value::String("human".to_string()));
        // external_tool_use_id not included

        let values = <CheckpointValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(values.external_tool_use_id, None); // not set
    }
}

/// Value positions for "agent_trace" event.
pub mod agent_trace_pos {
    pub const EVENT_TYPE: usize = 0; // String - "user_message", "assistant_message", "tool_use"
    pub const EVENT_TS: usize = 1; // u64 - event timestamp (nullable)
    pub const TOOL_USE_ID: usize = 2; // String - trace_id for this event (nullable)
    pub const TOOL_NAME: usize = 3; // String - for tool_use events (nullable)
    pub const PROMPT_TEXT: usize = 4; // String - user message text (nullable)
    pub const RESPONSE_TEXT: usize = 5; // String - assistant message text (nullable)
    pub const INPUT_TOKENS: usize = 6; // u64 - token usage (nullable)
    pub const OUTPUT_TOKENS: usize = 7; // u64 - token usage (nullable)
    pub const CACHE_READ_INPUT_TOKENS: usize = 8; // u64 - token usage (nullable)
    pub const CACHE_CREATION_INPUT_TOKENS: usize = 9; // u64 - token usage (nullable)
    pub const COST_USD: usize = 10; // f64 - cost in USD (nullable)
}

/// Values for Event ID 5: agent_trace
///
/// Recorded for each event extracted from an AI agent transcript.
/// Uses EventAttributes for session_id, trace_id, tool, model metadata.
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | event_type | String |
/// | 1 | event_ts | u64 (nullable) |
/// | 2 | external_tool_use_id | String (nullable) |
/// | 3 | tool_name | String (nullable) |
/// | 4 | prompt_text | String (nullable) |
/// | 5 | response_text | String (nullable) |
/// | 6 | input_tokens | u64 (nullable) |
/// | 7 | output_tokens | u64 (nullable) |
/// | 8 | cache_read_input_tokens | u64 (nullable) |
/// | 9 | cache_creation_input_tokens | u64 (nullable) |
/// | 10 | cost_usd | f64 (nullable) |
#[derive(Debug, Clone, Default)]
pub struct AgentTraceValues {
    pub event_type: PosField<String>,
    pub event_ts: PosField<u64>,
    pub external_tool_use_id: PosField<String>,
    pub tool_name: PosField<String>,
    pub prompt_text: PosField<String>,
    pub response_text: PosField<String>,
    pub input_tokens: PosField<u64>,
    pub output_tokens: PosField<u64>,
    pub cache_read_input_tokens: PosField<u64>,
    pub cache_creation_input_tokens: PosField<u64>,
    pub cost_usd: PosField<f64>,
}

impl AgentTraceValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn event_type(mut self, value: impl Into<String>) -> Self {
        self.event_type = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn event_type_null(mut self) -> Self {
        self.event_type = Some(None);
        self
    }

    pub fn event_ts(mut self, value: u64) -> Self {
        self.event_ts = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn event_ts_null(mut self) -> Self {
        self.event_ts = Some(None);
        self
    }

    pub fn external_tool_use_id(mut self, value: impl Into<String>) -> Self {
        self.external_tool_use_id = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn external_tool_use_id_null(mut self) -> Self {
        self.external_tool_use_id = Some(None);
        self
    }

    pub fn tool_name(mut self, value: impl Into<String>) -> Self {
        self.tool_name = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn tool_name_null(mut self) -> Self {
        self.tool_name = Some(None);
        self
    }

    pub fn prompt_text(mut self, value: impl Into<String>) -> Self {
        self.prompt_text = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn prompt_text_null(mut self) -> Self {
        self.prompt_text = Some(None);
        self
    }

    pub fn response_text(mut self, value: impl Into<String>) -> Self {
        self.response_text = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn response_text_null(mut self) -> Self {
        self.response_text = Some(None);
        self
    }

    pub fn input_tokens(mut self, value: u64) -> Self {
        self.input_tokens = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn input_tokens_null(mut self) -> Self {
        self.input_tokens = Some(None);
        self
    }

    pub fn output_tokens(mut self, value: u64) -> Self {
        self.output_tokens = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn output_tokens_null(mut self) -> Self {
        self.output_tokens = Some(None);
        self
    }

    pub fn cache_read_input_tokens(mut self, value: u64) -> Self {
        self.cache_read_input_tokens = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn cache_read_input_tokens_null(mut self) -> Self {
        self.cache_read_input_tokens = Some(None);
        self
    }

    pub fn cache_creation_input_tokens(mut self, value: u64) -> Self {
        self.cache_creation_input_tokens = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn cache_creation_input_tokens_null(mut self) -> Self {
        self.cache_creation_input_tokens = Some(None);
        self
    }

    pub fn cost_usd(mut self, value: f64) -> Self {
        self.cost_usd = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn cost_usd_null(mut self) -> Self {
        self.cost_usd = Some(None);
        self
    }
}

impl PosEncoded for AgentTraceValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();

        sparse_set(
            &mut map,
            agent_trace_pos::EVENT_TYPE,
            string_to_json(&self.event_type),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::EVENT_TS,
            u64_to_json(&self.event_ts),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::TOOL_USE_ID,
            string_to_json(&self.external_tool_use_id),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::TOOL_NAME,
            string_to_json(&self.tool_name),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::PROMPT_TEXT,
            string_to_json(&self.prompt_text),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::RESPONSE_TEXT,
            string_to_json(&self.response_text),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::INPUT_TOKENS,
            u64_to_json(&self.input_tokens),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::OUTPUT_TOKENS,
            u64_to_json(&self.output_tokens),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::CACHE_READ_INPUT_TOKENS,
            u64_to_json(&self.cache_read_input_tokens),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::CACHE_CREATION_INPUT_TOKENS,
            u64_to_json(&self.cache_creation_input_tokens),
        );
        sparse_set(
            &mut map,
            agent_trace_pos::COST_USD,
            f64_to_json(&self.cost_usd),
        );

        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            event_type: sparse_get_string(arr, agent_trace_pos::EVENT_TYPE),
            event_ts: sparse_get_u64(arr, agent_trace_pos::EVENT_TS),
            external_tool_use_id: sparse_get_string(arr, agent_trace_pos::TOOL_USE_ID),
            tool_name: sparse_get_string(arr, agent_trace_pos::TOOL_NAME),
            prompt_text: sparse_get_string(arr, agent_trace_pos::PROMPT_TEXT),
            response_text: sparse_get_string(arr, agent_trace_pos::RESPONSE_TEXT),
            input_tokens: sparse_get_u64(arr, agent_trace_pos::INPUT_TOKENS),
            output_tokens: sparse_get_u64(arr, agent_trace_pos::OUTPUT_TOKENS),
            cache_read_input_tokens: sparse_get_u64(arr, agent_trace_pos::CACHE_READ_INPUT_TOKENS),
            cache_creation_input_tokens: sparse_get_u64(arr, agent_trace_pos::CACHE_CREATION_INPUT_TOKENS),
            cost_usd: sparse_get_f64(arr, agent_trace_pos::COST_USD),
        }
    }
}

impl EventValues for AgentTraceValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentTrace
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}
