//! Event-specific value structs for metrics.

use super::pos_encoded::{
    sparse_get_string, sparse_get_u32, sparse_get_vec_string, sparse_get_vec_u32,
    sparse_get_vec_u64, sparse_set, string_to_json, u32_to_json, vec_string_to_json,
    vec_u32_to_json, vec_u64_to_json, PosEncoded, PosField,
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
    pub const TIME_WAITING_FOR_AI: usize = 9;
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
/// | 3 | tool_model_pairs | Vec<String> |
/// | 4 | mixed_additions | Vec<u32> |
/// | 5 | ai_additions | Vec<u32> |
/// | 6 | ai_accepted | Vec<u32> |
/// | 7 | total_ai_additions | Vec<u32> |
/// | 8 | total_ai_deletions | Vec<u32> |
/// | 9 | time_waiting_for_ai | Vec<u64> |
#[derive(Debug, Clone, Default)]
pub struct CommittedValues {
    // Scalar fields
    pub human_additions: PosField<u32>,
    pub git_diff_deleted_lines: PosField<u32>,
    pub git_diff_added_lines: PosField<u32>,

    // Array fields (parallel arrays)
    pub tool_model_pairs: PosField<Vec<String>>,
    pub mixed_additions: PosField<Vec<u32>>,
    pub ai_additions: PosField<Vec<u32>>,
    pub ai_accepted: PosField<Vec<u32>>,
    pub total_ai_additions: PosField<Vec<u32>>,
    pub total_ai_deletions: PosField<Vec<u32>>,
    pub time_waiting_for_ai: PosField<Vec<u64>>,
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

    pub fn human_additions_null(mut self) -> Self {
        self.human_additions = Some(None);
        self
    }

    pub fn git_diff_deleted_lines(mut self, value: u32) -> Self {
        self.git_diff_deleted_lines = Some(Some(value));
        self
    }

    pub fn git_diff_deleted_lines_null(mut self) -> Self {
        self.git_diff_deleted_lines = Some(None);
        self
    }

    pub fn git_diff_added_lines(mut self, value: u32) -> Self {
        self.git_diff_added_lines = Some(Some(value));
        self
    }

    pub fn git_diff_added_lines_null(mut self) -> Self {
        self.git_diff_added_lines = Some(None);
        self
    }

    // Builder methods for array fields

    pub fn tool_model_pairs(mut self, value: Vec<String>) -> Self {
        self.tool_model_pairs = Some(Some(value));
        self
    }

    pub fn tool_model_pairs_null(mut self) -> Self {
        self.tool_model_pairs = Some(None);
        self
    }

    pub fn mixed_additions(mut self, value: Vec<u32>) -> Self {
        self.mixed_additions = Some(Some(value));
        self
    }

    pub fn mixed_additions_null(mut self) -> Self {
        self.mixed_additions = Some(None);
        self
    }

    pub fn ai_additions(mut self, value: Vec<u32>) -> Self {
        self.ai_additions = Some(Some(value));
        self
    }

    pub fn ai_additions_null(mut self) -> Self {
        self.ai_additions = Some(None);
        self
    }

    pub fn ai_accepted(mut self, value: Vec<u32>) -> Self {
        self.ai_accepted = Some(Some(value));
        self
    }

    pub fn ai_accepted_null(mut self) -> Self {
        self.ai_accepted = Some(None);
        self
    }

    pub fn total_ai_additions(mut self, value: Vec<u32>) -> Self {
        self.total_ai_additions = Some(Some(value));
        self
    }

    pub fn total_ai_additions_null(mut self) -> Self {
        self.total_ai_additions = Some(None);
        self
    }

    pub fn total_ai_deletions(mut self, value: Vec<u32>) -> Self {
        self.total_ai_deletions = Some(Some(value));
        self
    }

    pub fn total_ai_deletions_null(mut self) -> Self {
        self.total_ai_deletions = Some(None);
        self
    }

    pub fn time_waiting_for_ai(mut self, value: Vec<u64>) -> Self {
        self.time_waiting_for_ai = Some(Some(value));
        self
    }

    pub fn time_waiting_for_ai_null(mut self) -> Self {
        self.time_waiting_for_ai = Some(None);
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
            committed_pos::MIXED_ADDITIONS,
            vec_u32_to_json(&self.mixed_additions),
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
        sparse_set(
            &mut map,
            committed_pos::TOTAL_AI_ADDITIONS,
            vec_u32_to_json(&self.total_ai_additions),
        );
        sparse_set(
            &mut map,
            committed_pos::TOTAL_AI_DELETIONS,
            vec_u32_to_json(&self.total_ai_deletions),
        );
        sparse_set(
            &mut map,
            committed_pos::TIME_WAITING_FOR_AI,
            vec_u64_to_json(&self.time_waiting_for_ai),
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
            mixed_additions: sparse_get_vec_u32(arr, committed_pos::MIXED_ADDITIONS),
            ai_additions: sparse_get_vec_u32(arr, committed_pos::AI_ADDITIONS),
            ai_accepted: sparse_get_vec_u32(arr, committed_pos::AI_ACCEPTED),
            total_ai_additions: sparse_get_vec_u32(arr, committed_pos::TOTAL_AI_ADDITIONS),
            total_ai_deletions: sparse_get_vec_u32(arr, committed_pos::TOTAL_AI_DELETIONS),
            time_waiting_for_ai: sparse_get_vec_u64(arr, committed_pos::TIME_WAITING_FOR_AI),
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
            .mixed_additions(vec![30, 20])
            .ai_additions(vec![100, 70])
            .ai_accepted(vec![80, 55])
            .total_ai_additions(vec![120, 80])
            .total_ai_deletions(vec![25, 15])
            .time_waiting_for_ai(vec![5000, 3000]);

        assert_eq!(values.human_additions, Some(Some(50)));
        assert_eq!(
            values.tool_model_pairs,
            Some(Some(vec!["all".to_string(), "claude-code:claude-3".to_string()]))
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
}
