//! Event-specific value structs for metrics.

use super::pos_encoded::{
    PosEncoded, PosField, sparse_get_string, sparse_get_u32, sparse_get_u64, sparse_get_vec_string,
    sparse_get_vec_u32, sparse_get_vec_u64, sparse_set, string_to_json, u32_to_json, u64_to_json,
    vec_string_to_json, vec_u32_to_json, vec_u64_to_json,
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
/// | 4 | mixed_additions | `Vec<u32>` |
/// | 5 | ai_additions | `Vec<u32>` |
/// | 6 | ai_accepted | `Vec<u32>` |
/// | 7 | total_ai_additions | `Vec<u32>` |
/// | 8 | total_ai_deletions | `Vec<u32>` |
/// | 9 | time_waiting_for_ai | `Vec<u64>` |
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
    pub mixed_additions: PosField<Vec<u32>>,
    pub ai_additions: PosField<Vec<u32>>,
    pub ai_accepted: PosField<Vec<u32>>,
    pub total_ai_additions: PosField<Vec<u32>>,
    pub total_ai_deletions: PosField<Vec<u32>>,
    pub time_waiting_for_ai: PosField<Vec<u64>>,

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

    pub fn mixed_additions(mut self, value: Vec<u32>) -> Self {
        self.mixed_additions = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn mixed_additions_null(mut self) -> Self {
        self.mixed_additions = Some(None);
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

    pub fn total_ai_additions(mut self, value: Vec<u32>) -> Self {
        self.total_ai_additions = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn total_ai_additions_null(mut self) -> Self {
        self.total_ai_additions = Some(None);
        self
    }

    pub fn total_ai_deletions(mut self, value: Vec<u32>) -> Self {
        self.total_ai_deletions = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn total_ai_deletions_null(mut self) -> Self {
        self.total_ai_deletions = Some(None);
        self
    }

    pub fn time_waiting_for_ai(mut self, value: Vec<u64>) -> Self {
        self.time_waiting_for_ai = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn time_waiting_for_ai_null(mut self) -> Self {
        self.time_waiting_for_ai = Some(None);
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
            mixed_additions: sparse_get_vec_u32(arr, committed_pos::MIXED_ADDITIONS),
            ai_additions: sparse_get_vec_u32(arr, committed_pos::AI_ADDITIONS),
            ai_accepted: sparse_get_vec_u32(arr, committed_pos::AI_ACCEPTED),
            total_ai_additions: sparse_get_vec_u32(arr, committed_pos::TOTAL_AI_ADDITIONS),
            total_ai_deletions: sparse_get_vec_u32(arr, committed_pos::TOTAL_AI_DELETIONS),
            time_waiting_for_ai: sparse_get_vec_u64(arr, committed_pos::TIME_WAITING_FOR_AI),

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
#[derive(Debug, Clone, Default)]
pub struct CheckpointValues {
    pub checkpoint_ts: PosField<u64>,
    pub kind: PosField<String>,
    pub file_path: PosField<String>,
    pub lines_added: PosField<u32>,
    pub lines_deleted: PosField<u32>,
    pub lines_added_sloc: PosField<u32>,
    pub lines_deleted_sloc: PosField<u32>,
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

/// Value positions for "tool_call" event.
pub mod tool_call_pos {
    pub const TOOL_NAME: usize = 0;
}

/// Values for Event ID 5: tool_call
///
/// Recorded when an agent invokes a tool (e.g. Write, Edit, Bash, Read).
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | tool_name | String |
#[derive(Debug, Clone, Default)]
pub struct ToolCallValues {
    pub tool_name: PosField<String>,
}

impl ToolCallValues {
    pub fn new() -> Self {
        Self::default()
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
}

impl PosEncoded for ToolCallValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            tool_call_pos::TOOL_NAME,
            string_to_json(&self.tool_name),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            tool_name: sparse_get_string(arr, tool_call_pos::TOOL_NAME),
        }
    }
}

impl EventValues for ToolCallValues {
    fn event_id() -> MetricEventId {
        MetricEventId::ToolCall
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "mcp_invocation" event.
pub mod mcp_invocation_pos {
    pub const SERVER_NAME: usize = 0;
    pub const TOOL_NAME: usize = 1;
}

/// Values for Event ID 6: mcp_invocation
///
/// Recorded when an agent invokes an MCP server tool.
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | server_name | String |
/// | 1 | tool_name | String |
#[derive(Debug, Clone, Default)]
pub struct McpInvocationValues {
    pub server_name: PosField<String>,
    pub tool_name: PosField<String>,
}

impl McpInvocationValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn server_name(mut self, value: impl Into<String>) -> Self {
        self.server_name = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn server_name_null(mut self) -> Self {
        self.server_name = Some(None);
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
}

impl PosEncoded for McpInvocationValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            mcp_invocation_pos::SERVER_NAME,
            string_to_json(&self.server_name),
        );
        sparse_set(
            &mut map,
            mcp_invocation_pos::TOOL_NAME,
            string_to_json(&self.tool_name),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            server_name: sparse_get_string(arr, mcp_invocation_pos::SERVER_NAME),
            tool_name: sparse_get_string(arr, mcp_invocation_pos::TOOL_NAME),
        }
    }
}

impl EventValues for McpInvocationValues {
    fn event_id() -> MetricEventId {
        MetricEventId::McpInvocation
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "new_message" event.
pub mod new_message_pos {
    pub const ROLE: usize = 0;
}

/// Values for Event ID 7: new_message
///
/// Recorded when a new human or AI message occurs in an agent session.
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | role | String ("human" or "ai") |
#[derive(Debug, Clone, Default)]
pub struct NewMessageValues {
    pub role: PosField<String>,
}

impl NewMessageValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn role(mut self, value: impl Into<String>) -> Self {
        self.role = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn role_null(mut self) -> Self {
        self.role = Some(None);
        self
    }
}

impl PosEncoded for NewMessageValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(&mut map, new_message_pos::ROLE, string_to_json(&self.role));
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            role: sparse_get_string(arr, new_message_pos::ROLE),
        }
    }
}

impl EventValues for NewMessageValues {
    fn event_id() -> MetricEventId {
        MetricEventId::NewMessage
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "skill_used" event.
pub mod skill_used_pos {
    pub const SKILL_NAME: usize = 0;
}

/// Values for Event ID 8: skill_used
///
/// Recorded when a skill or custom command is invoked by an agent.
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | skill_name | String |
#[derive(Debug, Clone, Default)]
pub struct SkillUsedValues {
    pub skill_name: PosField<String>,
}

impl SkillUsedValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn skill_name(mut self, value: impl Into<String>) -> Self {
        self.skill_name = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn skill_name_null(mut self) -> Self {
        self.skill_name = Some(None);
        self
    }
}

impl PosEncoded for SkillUsedValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            skill_used_pos::SKILL_NAME,
            string_to_json(&self.skill_name),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            skill_name: sparse_get_string(arr, skill_used_pos::SKILL_NAME),
        }
    }
}

impl EventValues for SkillUsedValues {
    fn event_id() -> MetricEventId {
        MetricEventId::SkillUsed
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "subagent_event" event.
pub mod subagent_event_pos {
    pub const EVENT_TYPE: usize = 0;
    pub const SUBAGENT_ID: usize = 1;
    pub const SUBAGENT_MODEL: usize = 2;
}

/// Values for Event ID 9: subagent_event
///
/// Recorded when a subagent lifecycle event occurs (start/stop).
///
/// **Fields:**
/// | Position | Name | Type |
/// |----------|------|------|
/// | 0 | event_type | String ("start" or "stop") |
/// | 1 | subagent_id | String |
/// | 2 | subagent_model | String |
#[derive(Debug, Clone, Default)]
pub struct SubagentEventValues {
    pub event_type: PosField<String>,
    pub subagent_id: PosField<String>,
    pub subagent_model: PosField<String>,
}

impl SubagentEventValues {
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

    pub fn subagent_id(mut self, value: impl Into<String>) -> Self {
        self.subagent_id = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn subagent_id_null(mut self) -> Self {
        self.subagent_id = Some(None);
        self
    }

    pub fn subagent_model(mut self, value: impl Into<String>) -> Self {
        self.subagent_model = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn subagent_model_null(mut self) -> Self {
        self.subagent_model = Some(None);
        self
    }
}

impl PosEncoded for SubagentEventValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            subagent_event_pos::EVENT_TYPE,
            string_to_json(&self.event_type),
        );
        sparse_set(
            &mut map,
            subagent_event_pos::SUBAGENT_ID,
            string_to_json(&self.subagent_id),
        );
        sparse_set(
            &mut map,
            subagent_event_pos::SUBAGENT_MODEL,
            string_to_json(&self.subagent_model),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            event_type: sparse_get_string(arr, subagent_event_pos::EVENT_TYPE),
            subagent_id: sparse_get_string(arr, subagent_event_pos::SUBAGENT_ID),
            subagent_model: sparse_get_string(arr, subagent_event_pos::SUBAGENT_MODEL),
        }
    }
}

impl EventValues for SubagentEventValues {
    fn event_id() -> MetricEventId {
        MetricEventId::SubagentEvent
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
            .mixed_additions(vec![10, 5])
            .ai_additions(vec![100, 50])
            .ai_accepted(vec![80, 40])
            .total_ai_additions(vec![120, 60])
            .total_ai_deletions(vec![20, 10])
            .time_waiting_for_ai(vec![5000, 3000]);

        assert_eq!(
            values.tool_model_pairs,
            Some(Some(vec!["all".to_string(), "cursor:gpt-4".to_string()]))
        );
        assert_eq!(values.mixed_additions, Some(Some(vec![10, 5])));
        assert_eq!(values.ai_additions, Some(Some(vec![100, 50])));
        assert_eq!(values.ai_accepted, Some(Some(vec![80, 40])));
        assert_eq!(values.total_ai_additions, Some(Some(vec![120, 60])));
        assert_eq!(values.total_ai_deletions, Some(Some(vec![20, 10])));
        assert_eq!(values.time_waiting_for_ai, Some(Some(vec![5000, 3000])));
    }

    #[test]
    fn test_committed_values_array_nulls() {
        let values = CommittedValues::new()
            .mixed_additions_null()
            .ai_accepted_null()
            .total_ai_additions_null()
            .total_ai_deletions_null()
            .time_waiting_for_ai_null();

        assert_eq!(values.mixed_additions, Some(None));
        assert_eq!(values.ai_accepted, Some(None));
        assert_eq!(values.total_ai_additions, Some(None));
        assert_eq!(values.total_ai_deletions, Some(None));
        assert_eq!(values.time_waiting_for_ai, Some(None));
    }

    #[test]
    fn test_tool_call_values_builder() {
        let values = ToolCallValues::new().tool_name("Write");
        assert_eq!(values.tool_name, Some(Some("Write".to_string())));
    }

    #[test]
    fn test_tool_call_values_to_sparse() {
        use super::PosEncoded;
        let values = ToolCallValues::new().tool_name("Edit");
        let sparse = PosEncoded::to_sparse(&values);
        assert_eq!(sparse.get("0"), Some(&Value::String("Edit".to_string())));
    }

    #[test]
    fn test_tool_call_values_from_sparse() {
        use super::PosEncoded;
        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("Bash".to_string()));
        let values = <ToolCallValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(values.tool_name, Some(Some("Bash".to_string())));
    }

    #[test]
    fn test_tool_call_values_roundtrip() {
        use super::PosEncoded;
        let original = ToolCallValues::new().tool_name("Read");
        let sparse = PosEncoded::to_sparse(&original);
        let restored = <ToolCallValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(restored.tool_name, Some(Some("Read".to_string())));
    }

    #[test]
    fn test_tool_call_event_id() {
        assert_eq!(ToolCallValues::event_id(), MetricEventId::ToolCall);
        assert_eq!(ToolCallValues::event_id() as u16, 5);
    }

    #[test]
    fn test_mcp_invocation_values_builder() {
        let values = McpInvocationValues::new()
            .server_name("filesystem")
            .tool_name("read_file");
        assert_eq!(values.server_name, Some(Some("filesystem".to_string())));
        assert_eq!(values.tool_name, Some(Some("read_file".to_string())));
    }

    #[test]
    fn test_mcp_invocation_values_to_sparse() {
        use super::PosEncoded;
        let values = McpInvocationValues::new()
            .server_name("github")
            .tool_name("create_issue");
        let sparse = PosEncoded::to_sparse(&values);
        assert_eq!(sparse.get("0"), Some(&Value::String("github".to_string())));
        assert_eq!(
            sparse.get("1"),
            Some(&Value::String("create_issue".to_string()))
        );
    }

    #[test]
    fn test_mcp_invocation_values_from_sparse() {
        use super::PosEncoded;
        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("slack".to_string()));
        sparse.insert("1".to_string(), Value::String("post_message".to_string()));
        let values = <McpInvocationValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(values.server_name, Some(Some("slack".to_string())));
        assert_eq!(values.tool_name, Some(Some("post_message".to_string())));
    }

    #[test]
    fn test_mcp_invocation_values_roundtrip() {
        use super::PosEncoded;
        let original = McpInvocationValues::new()
            .server_name("db")
            .tool_name("query");
        let sparse = PosEncoded::to_sparse(&original);
        let restored = <McpInvocationValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(restored.server_name, Some(Some("db".to_string())));
        assert_eq!(restored.tool_name, Some(Some("query".to_string())));
    }

    #[test]
    fn test_mcp_invocation_event_id() {
        assert_eq!(
            McpInvocationValues::event_id(),
            MetricEventId::McpInvocation
        );
        assert_eq!(McpInvocationValues::event_id() as u16, 6);
    }

    #[test]
    fn test_new_message_values_builder() {
        let values = NewMessageValues::new().role("human");
        assert_eq!(values.role, Some(Some("human".to_string())));
    }

    #[test]
    fn test_new_message_values_to_sparse() {
        use super::PosEncoded;
        let values = NewMessageValues::new().role("ai");
        let sparse = PosEncoded::to_sparse(&values);
        assert_eq!(sparse.get("0"), Some(&Value::String("ai".to_string())));
    }

    #[test]
    fn test_new_message_values_from_sparse() {
        use super::PosEncoded;
        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("human".to_string()));
        let values = <NewMessageValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(values.role, Some(Some("human".to_string())));
    }

    #[test]
    fn test_new_message_values_roundtrip() {
        use super::PosEncoded;
        let original = NewMessageValues::new().role("ai");
        let sparse = PosEncoded::to_sparse(&original);
        let restored = <NewMessageValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(restored.role, Some(Some("ai".to_string())));
    }

    #[test]
    fn test_new_message_event_id() {
        assert_eq!(NewMessageValues::event_id(), MetricEventId::NewMessage);
        assert_eq!(NewMessageValues::event_id() as u16, 7);
    }

    #[test]
    fn test_skill_used_values_builder() {
        let values = SkillUsedValues::new().skill_name("deploy");
        assert_eq!(values.skill_name, Some(Some("deploy".to_string())));
    }

    #[test]
    fn test_skill_used_values_to_sparse() {
        use super::PosEncoded;
        let values = SkillUsedValues::new().skill_name("test-runner");
        let sparse = PosEncoded::to_sparse(&values);
        assert_eq!(
            sparse.get("0"),
            Some(&Value::String("test-runner".to_string()))
        );
    }

    #[test]
    fn test_skill_used_values_from_sparse() {
        use super::PosEncoded;
        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("lint".to_string()));
        let values = <SkillUsedValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(values.skill_name, Some(Some("lint".to_string())));
    }

    #[test]
    fn test_skill_used_values_roundtrip() {
        use super::PosEncoded;
        let original = SkillUsedValues::new().skill_name("format");
        let sparse = PosEncoded::to_sparse(&original);
        let restored = <SkillUsedValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(restored.skill_name, Some(Some("format".to_string())));
    }

    #[test]
    fn test_skill_used_event_id() {
        assert_eq!(SkillUsedValues::event_id(), MetricEventId::SkillUsed);
        assert_eq!(SkillUsedValues::event_id() as u16, 8);
    }

    #[test]
    fn test_subagent_event_values_builder() {
        let values = SubagentEventValues::new()
            .event_type("start")
            .subagent_id("agent-123")
            .subagent_model("claude-3-haiku");
        assert_eq!(values.event_type, Some(Some("start".to_string())));
        assert_eq!(values.subagent_id, Some(Some("agent-123".to_string())));
        assert_eq!(
            values.subagent_model,
            Some(Some("claude-3-haiku".to_string()))
        );
    }

    #[test]
    fn test_subagent_event_values_to_sparse() {
        use super::PosEncoded;
        let values = SubagentEventValues::new()
            .event_type("stop")
            .subagent_id("worker-1")
            .subagent_model("claude-3-sonnet");
        let sparse = PosEncoded::to_sparse(&values);
        assert_eq!(sparse.get("0"), Some(&Value::String("stop".to_string())));
        assert_eq!(
            sparse.get("1"),
            Some(&Value::String("worker-1".to_string()))
        );
        assert_eq!(
            sparse.get("2"),
            Some(&Value::String("claude-3-sonnet".to_string()))
        );
    }

    #[test]
    fn test_subagent_event_values_from_sparse() {
        use super::PosEncoded;
        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("start".to_string()));
        sparse.insert("1".to_string(), Value::String("sub-456".to_string()));
        sparse.insert("2".to_string(), Value::String("claude-3-opus".to_string()));
        let values = <SubagentEventValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(values.event_type, Some(Some("start".to_string())));
        assert_eq!(values.subagent_id, Some(Some("sub-456".to_string())));
        assert_eq!(
            values.subagent_model,
            Some(Some("claude-3-opus".to_string()))
        );
    }

    #[test]
    fn test_subagent_event_values_roundtrip() {
        use super::PosEncoded;
        let original = SubagentEventValues::new()
            .event_type("start")
            .subagent_id("agent-abc")
            .subagent_model("gpt-4");
        let sparse = PosEncoded::to_sparse(&original);
        let restored = <SubagentEventValues as PosEncoded>::from_sparse(&sparse);
        assert_eq!(restored.event_type, Some(Some("start".to_string())));
        assert_eq!(restored.subagent_id, Some(Some("agent-abc".to_string())));
        assert_eq!(restored.subagent_model, Some(Some("gpt-4".to_string())));
    }

    #[test]
    fn test_subagent_event_id() {
        assert_eq!(
            SubagentEventValues::event_id(),
            MetricEventId::SubagentEvent
        );
        assert_eq!(SubagentEventValues::event_id() as u16, 9);
    }

    #[test]
    fn test_subagent_event_values_with_nulls() {
        let values = SubagentEventValues::new()
            .event_type("start")
            .subagent_id_null()
            .subagent_model_null();
        assert_eq!(values.event_type, Some(Some("start".to_string())));
        assert_eq!(values.subagent_id, Some(None));
        assert_eq!(values.subagent_model, Some(None));
    }
}
