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

/// Value positions for "agent_session" event.
pub mod agent_session_pos {
    pub const PHASE: usize = 0; // String - started|ended
    pub const REASON: usize = 1; // String
    pub const SOURCE: usize = 2; // String
    pub const MODE: usize = 3; // String
    pub const DURATION_MS: usize = 4; // u64
    pub const IS_INFERRED: usize = 5; // u32 (0|1)
}

#[derive(Debug, Clone, Default)]
pub struct AgentSessionValues {
    pub phase: PosField<String>,
    pub reason: PosField<String>,
    pub source: PosField<String>,
    pub mode: PosField<String>,
    pub duration_ms: PosField<u64>,
    pub is_inferred: PosField<u32>,
}

impl AgentSessionValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn phase(mut self, value: impl Into<String>) -> Self {
        self.phase = Some(Some(value.into()));
        self
    }

    pub fn reason(mut self, value: impl Into<String>) -> Self {
        self.reason = Some(Some(value.into()));
        self
    }

    pub fn source(mut self, value: impl Into<String>) -> Self {
        self.source = Some(Some(value.into()));
        self
    }

    pub fn mode(mut self, value: impl Into<String>) -> Self {
        self.mode = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn duration_ms(mut self, value: u64) -> Self {
        self.duration_ms = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn inferred(mut self, value: u32) -> Self {
        self.is_inferred = Some(Some(value));
        self
    }
}

impl PosEncoded for AgentSessionValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            agent_session_pos::PHASE,
            string_to_json(&self.phase),
        );
        sparse_set(
            &mut map,
            agent_session_pos::REASON,
            string_to_json(&self.reason),
        );
        sparse_set(
            &mut map,
            agent_session_pos::SOURCE,
            string_to_json(&self.source),
        );
        sparse_set(
            &mut map,
            agent_session_pos::MODE,
            string_to_json(&self.mode),
        );
        sparse_set(
            &mut map,
            agent_session_pos::DURATION_MS,
            u64_to_json(&self.duration_ms),
        );
        sparse_set(
            &mut map,
            agent_session_pos::IS_INFERRED,
            u32_to_json(&self.is_inferred),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            phase: sparse_get_string(arr, agent_session_pos::PHASE),
            reason: sparse_get_string(arr, agent_session_pos::REASON),
            source: sparse_get_string(arr, agent_session_pos::SOURCE),
            mode: sparse_get_string(arr, agent_session_pos::MODE),
            duration_ms: sparse_get_u64(arr, agent_session_pos::DURATION_MS),
            is_inferred: sparse_get_u32(arr, agent_session_pos::IS_INFERRED),
        }
    }
}

impl EventValues for AgentSessionValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentSession
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "agent_message" event.
pub mod agent_message_pos {
    pub const ROLE: usize = 0; // String
    pub const PROMPT_CHAR_COUNT: usize = 1; // u32
    pub const ATTACHMENT_COUNT: usize = 2; // u32
}

#[derive(Debug, Clone, Default)]
pub struct AgentMessageValues {
    pub role: PosField<String>,
    pub prompt_char_count: PosField<u32>,
    pub attachment_count: PosField<u32>,
}

impl AgentMessageValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn role(mut self, value: impl Into<String>) -> Self {
        self.role = Some(Some(value.into()));
        self
    }

    pub fn prompt_char_count(mut self, value: u32) -> Self {
        self.prompt_char_count = Some(Some(value));
        self
    }

    pub fn attachment_count(mut self, value: u32) -> Self {
        self.attachment_count = Some(Some(value));
        self
    }
}

impl PosEncoded for AgentMessageValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            agent_message_pos::ROLE,
            string_to_json(&self.role),
        );
        sparse_set(
            &mut map,
            agent_message_pos::PROMPT_CHAR_COUNT,
            u32_to_json(&self.prompt_char_count),
        );
        sparse_set(
            &mut map,
            agent_message_pos::ATTACHMENT_COUNT,
            u32_to_json(&self.attachment_count),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            role: sparse_get_string(arr, agent_message_pos::ROLE),
            prompt_char_count: sparse_get_u32(arr, agent_message_pos::PROMPT_CHAR_COUNT),
            attachment_count: sparse_get_u32(arr, agent_message_pos::ATTACHMENT_COUNT),
        }
    }
}

impl EventValues for AgentMessageValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentMessage
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "agent_response" event.
pub mod agent_response_pos {
    pub const PHASE: usize = 0; // String - started|ended
    pub const REASON: usize = 1; // String
    pub const STATUS: usize = 2; // String
    pub const RESPONSE_CHAR_COUNT: usize = 3; // u32
    pub const IS_INFERRED: usize = 4; // u32 (0|1)
}

#[derive(Debug, Clone, Default)]
pub struct AgentResponseValues {
    pub phase: PosField<String>,
    pub reason: PosField<String>,
    pub status: PosField<String>,
    pub response_char_count: PosField<u32>,
    pub is_inferred: PosField<u32>,
}

impl AgentResponseValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn phase(mut self, value: impl Into<String>) -> Self {
        self.phase = Some(Some(value.into()));
        self
    }

    pub fn reason(mut self, value: impl Into<String>) -> Self {
        self.reason = Some(Some(value.into()));
        self
    }

    pub fn status(mut self, value: impl Into<String>) -> Self {
        self.status = Some(Some(value.into()));
        self
    }

    pub fn response_char_count(mut self, value: u32) -> Self {
        self.response_char_count = Some(Some(value));
        self
    }

    pub fn inferred(mut self, value: u32) -> Self {
        self.is_inferred = Some(Some(value));
        self
    }
}

impl PosEncoded for AgentResponseValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            agent_response_pos::PHASE,
            string_to_json(&self.phase),
        );
        sparse_set(
            &mut map,
            agent_response_pos::REASON,
            string_to_json(&self.reason),
        );
        sparse_set(
            &mut map,
            agent_response_pos::STATUS,
            string_to_json(&self.status),
        );
        sparse_set(
            &mut map,
            agent_response_pos::RESPONSE_CHAR_COUNT,
            u32_to_json(&self.response_char_count),
        );
        sparse_set(
            &mut map,
            agent_response_pos::IS_INFERRED,
            u32_to_json(&self.is_inferred),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            phase: sparse_get_string(arr, agent_response_pos::PHASE),
            reason: sparse_get_string(arr, agent_response_pos::REASON),
            status: sparse_get_string(arr, agent_response_pos::STATUS),
            response_char_count: sparse_get_u32(arr, agent_response_pos::RESPONSE_CHAR_COUNT),
            is_inferred: sparse_get_u32(arr, agent_response_pos::IS_INFERRED),
        }
    }
}

impl EventValues for AgentResponseValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentResponse
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "agent_tool_call" event.
pub mod agent_tool_call_pos {
    pub const PHASE: usize = 0; // String - started|ended|failed|permission_requested
    pub const TOOL_NAME: usize = 1; // String
    pub const TOOL_USE_ID: usize = 2; // String
    pub const DURATION_MS: usize = 3; // u64
    pub const FAILURE_TYPE: usize = 4; // String
    pub const IS_INFERRED: usize = 5; // u32 (0|1)
}

#[derive(Debug, Clone, Default)]
pub struct AgentToolCallValues {
    pub phase: PosField<String>,
    pub tool_name: PosField<String>,
    pub tool_use_id: PosField<String>,
    pub duration_ms: PosField<u64>,
    pub failure_type: PosField<String>,
    pub is_inferred: PosField<u32>,
}

impl AgentToolCallValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn phase(mut self, value: impl Into<String>) -> Self {
        self.phase = Some(Some(value.into()));
        self
    }

    pub fn tool_name(mut self, value: impl Into<String>) -> Self {
        self.tool_name = Some(Some(value.into()));
        self
    }

    pub fn tool_use_id(mut self, value: impl Into<String>) -> Self {
        self.tool_use_id = Some(Some(value.into()));
        self
    }

    pub fn duration_ms(mut self, value: u64) -> Self {
        self.duration_ms = Some(Some(value));
        self
    }

    pub fn failure_type(mut self, value: impl Into<String>) -> Self {
        self.failure_type = Some(Some(value.into()));
        self
    }

    pub fn inferred(mut self, value: u32) -> Self {
        self.is_inferred = Some(Some(value));
        self
    }
}

impl PosEncoded for AgentToolCallValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            agent_tool_call_pos::PHASE,
            string_to_json(&self.phase),
        );
        sparse_set(
            &mut map,
            agent_tool_call_pos::TOOL_NAME,
            string_to_json(&self.tool_name),
        );
        sparse_set(
            &mut map,
            agent_tool_call_pos::TOOL_USE_ID,
            string_to_json(&self.tool_use_id),
        );
        sparse_set(
            &mut map,
            agent_tool_call_pos::DURATION_MS,
            u64_to_json(&self.duration_ms),
        );
        sparse_set(
            &mut map,
            agent_tool_call_pos::FAILURE_TYPE,
            string_to_json(&self.failure_type),
        );
        sparse_set(
            &mut map,
            agent_tool_call_pos::IS_INFERRED,
            u32_to_json(&self.is_inferred),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            phase: sparse_get_string(arr, agent_tool_call_pos::PHASE),
            tool_name: sparse_get_string(arr, agent_tool_call_pos::TOOL_NAME),
            tool_use_id: sparse_get_string(arr, agent_tool_call_pos::TOOL_USE_ID),
            duration_ms: sparse_get_u64(arr, agent_tool_call_pos::DURATION_MS),
            failure_type: sparse_get_string(arr, agent_tool_call_pos::FAILURE_TYPE),
            is_inferred: sparse_get_u32(arr, agent_tool_call_pos::IS_INFERRED),
        }
    }
}

impl EventValues for AgentToolCallValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentToolCall
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "agent_mcp_call" event.
pub mod agent_mcp_call_pos {
    pub const PHASE: usize = 0; // String - started|ended|failed|permission_requested
    pub const MCP_SERVER: usize = 1; // String
    pub const TOOL_NAME: usize = 2; // String
    pub const TRANSPORT: usize = 3; // String
    pub const DURATION_MS: usize = 4; // u64
    pub const FAILURE_TYPE: usize = 5; // String
    pub const IS_INFERRED: usize = 6; // u32 (0|1)
}

#[derive(Debug, Clone, Default)]
pub struct AgentMcpCallValues {
    pub phase: PosField<String>,
    pub mcp_server: PosField<String>,
    pub tool_name: PosField<String>,
    pub transport: PosField<String>,
    pub duration_ms: PosField<u64>,
    pub failure_type: PosField<String>,
    pub is_inferred: PosField<u32>,
}

impl AgentMcpCallValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn phase(mut self, value: impl Into<String>) -> Self {
        self.phase = Some(Some(value.into()));
        self
    }

    pub fn mcp_server(mut self, value: impl Into<String>) -> Self {
        self.mcp_server = Some(Some(value.into()));
        self
    }

    pub fn tool_name(mut self, value: impl Into<String>) -> Self {
        self.tool_name = Some(Some(value.into()));
        self
    }

    pub fn transport(mut self, value: impl Into<String>) -> Self {
        self.transport = Some(Some(value.into()));
        self
    }

    pub fn duration_ms(mut self, value: u64) -> Self {
        self.duration_ms = Some(Some(value));
        self
    }

    pub fn failure_type(mut self, value: impl Into<String>) -> Self {
        self.failure_type = Some(Some(value.into()));
        self
    }

    pub fn inferred(mut self, value: u32) -> Self {
        self.is_inferred = Some(Some(value));
        self
    }
}

impl PosEncoded for AgentMcpCallValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            agent_mcp_call_pos::PHASE,
            string_to_json(&self.phase),
        );
        sparse_set(
            &mut map,
            agent_mcp_call_pos::MCP_SERVER,
            string_to_json(&self.mcp_server),
        );
        sparse_set(
            &mut map,
            agent_mcp_call_pos::TOOL_NAME,
            string_to_json(&self.tool_name),
        );
        sparse_set(
            &mut map,
            agent_mcp_call_pos::TRANSPORT,
            string_to_json(&self.transport),
        );
        sparse_set(
            &mut map,
            agent_mcp_call_pos::DURATION_MS,
            u64_to_json(&self.duration_ms),
        );
        sparse_set(
            &mut map,
            agent_mcp_call_pos::FAILURE_TYPE,
            string_to_json(&self.failure_type),
        );
        sparse_set(
            &mut map,
            agent_mcp_call_pos::IS_INFERRED,
            u32_to_json(&self.is_inferred),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            phase: sparse_get_string(arr, agent_mcp_call_pos::PHASE),
            mcp_server: sparse_get_string(arr, agent_mcp_call_pos::MCP_SERVER),
            tool_name: sparse_get_string(arr, agent_mcp_call_pos::TOOL_NAME),
            transport: sparse_get_string(arr, agent_mcp_call_pos::TRANSPORT),
            duration_ms: sparse_get_u64(arr, agent_mcp_call_pos::DURATION_MS),
            failure_type: sparse_get_string(arr, agent_mcp_call_pos::FAILURE_TYPE),
            is_inferred: sparse_get_u32(arr, agent_mcp_call_pos::IS_INFERRED),
        }
    }
}

impl EventValues for AgentMcpCallValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentMcpCall
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "agent_skill_usage" event.
pub mod agent_skill_usage_pos {
    pub const SKILL_NAME: usize = 0; // String
    pub const DETECTION_METHOD: usize = 1; // String
    pub const IS_INFERRED: usize = 2; // u32 (0|1)
}

#[derive(Debug, Clone, Default)]
pub struct AgentSkillUsageValues {
    pub skill_name: PosField<String>,
    pub detection_method: PosField<String>,
    pub is_inferred: PosField<u32>,
}

impl AgentSkillUsageValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn skill_name(mut self, value: impl Into<String>) -> Self {
        self.skill_name = Some(Some(value.into()));
        self
    }

    pub fn detection_method(mut self, value: impl Into<String>) -> Self {
        self.detection_method = Some(Some(value.into()));
        self
    }

    pub fn inferred(mut self, value: u32) -> Self {
        self.is_inferred = Some(Some(value));
        self
    }
}

impl PosEncoded for AgentSkillUsageValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            agent_skill_usage_pos::SKILL_NAME,
            string_to_json(&self.skill_name),
        );
        sparse_set(
            &mut map,
            agent_skill_usage_pos::DETECTION_METHOD,
            string_to_json(&self.detection_method),
        );
        sparse_set(
            &mut map,
            agent_skill_usage_pos::IS_INFERRED,
            u32_to_json(&self.is_inferred),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            skill_name: sparse_get_string(arr, agent_skill_usage_pos::SKILL_NAME),
            detection_method: sparse_get_string(arr, agent_skill_usage_pos::DETECTION_METHOD),
            is_inferred: sparse_get_u32(arr, agent_skill_usage_pos::IS_INFERRED),
        }
    }
}

impl EventValues for AgentSkillUsageValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentSkillUsage
    }

    fn to_sparse(&self) -> SparseArray {
        PosEncoded::to_sparse(self)
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        PosEncoded::from_sparse(arr)
    }
}

/// Value positions for "agent_subagent" event.
pub mod agent_subagent_pos {
    pub const PHASE: usize = 0; // String - started|ended
    pub const SUBAGENT_ID: usize = 1; // String
    pub const SUBAGENT_TYPE: usize = 2; // String
    pub const STATUS: usize = 3; // String
    pub const DURATION_MS: usize = 4; // u64
    pub const RESULT_CHAR_COUNT: usize = 5; // u32
    pub const IS_INFERRED: usize = 6; // u32 (0|1)
}

#[derive(Debug, Clone, Default)]
pub struct AgentSubagentValues {
    pub phase: PosField<String>,
    pub subagent_id: PosField<String>,
    pub subagent_type: PosField<String>,
    pub status: PosField<String>,
    pub duration_ms: PosField<u64>,
    pub result_char_count: PosField<u32>,
    pub is_inferred: PosField<u32>,
}

impl AgentSubagentValues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn phase(mut self, value: impl Into<String>) -> Self {
        self.phase = Some(Some(value.into()));
        self
    }

    pub fn subagent_id(mut self, value: impl Into<String>) -> Self {
        self.subagent_id = Some(Some(value.into()));
        self
    }

    pub fn subagent_type(mut self, value: impl Into<String>) -> Self {
        self.subagent_type = Some(Some(value.into()));
        self
    }

    pub fn status(mut self, value: impl Into<String>) -> Self {
        self.status = Some(Some(value.into()));
        self
    }

    pub fn duration_ms(mut self, value: u64) -> Self {
        self.duration_ms = Some(Some(value));
        self
    }

    pub fn result_char_count(mut self, value: u32) -> Self {
        self.result_char_count = Some(Some(value));
        self
    }

    #[allow(dead_code)]
    pub fn inferred(mut self, value: u32) -> Self {
        self.is_inferred = Some(Some(value));
        self
    }
}

impl PosEncoded for AgentSubagentValues {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            agent_subagent_pos::PHASE,
            string_to_json(&self.phase),
        );
        sparse_set(
            &mut map,
            agent_subagent_pos::SUBAGENT_ID,
            string_to_json(&self.subagent_id),
        );
        sparse_set(
            &mut map,
            agent_subagent_pos::SUBAGENT_TYPE,
            string_to_json(&self.subagent_type),
        );
        sparse_set(
            &mut map,
            agent_subagent_pos::STATUS,
            string_to_json(&self.status),
        );
        sparse_set(
            &mut map,
            agent_subagent_pos::DURATION_MS,
            u64_to_json(&self.duration_ms),
        );
        sparse_set(
            &mut map,
            agent_subagent_pos::RESULT_CHAR_COUNT,
            u32_to_json(&self.result_char_count),
        );
        sparse_set(
            &mut map,
            agent_subagent_pos::IS_INFERRED,
            u32_to_json(&self.is_inferred),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            phase: sparse_get_string(arr, agent_subagent_pos::PHASE),
            subagent_id: sparse_get_string(arr, agent_subagent_pos::SUBAGENT_ID),
            subagent_type: sparse_get_string(arr, agent_subagent_pos::SUBAGENT_TYPE),
            status: sparse_get_string(arr, agent_subagent_pos::STATUS),
            duration_ms: sparse_get_u64(arr, agent_subagent_pos::DURATION_MS),
            result_char_count: sparse_get_u32(arr, agent_subagent_pos::RESULT_CHAR_COUNT),
            is_inferred: sparse_get_u32(arr, agent_subagent_pos::IS_INFERRED),
        }
    }
}

impl EventValues for AgentSubagentValues {
    fn event_id() -> MetricEventId {
        MetricEventId::AgentSubagent
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
    fn test_agent_session_values_roundtrip() {
        use super::PosEncoded;

        let values = AgentSessionValues::new()
            .phase("started")
            .source("interactive")
            .mode("agent")
            .inferred(0);
        let sparse = PosEncoded::to_sparse(&values);
        let restored = <AgentSessionValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.phase, Some(Some("started".to_string())));
        assert_eq!(restored.source, Some(Some("interactive".to_string())));
        assert_eq!(restored.mode, Some(Some("agent".to_string())));
        assert_eq!(restored.is_inferred, Some(Some(0)));
        assert_eq!(AgentSessionValues::event_id() as u16, 5);
    }

    #[test]
    fn test_agent_message_values_roundtrip() {
        use super::PosEncoded;

        let values = AgentMessageValues::new()
            .role("human")
            .prompt_char_count(128)
            .attachment_count(2);
        let sparse = PosEncoded::to_sparse(&values);
        let restored = <AgentMessageValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.role, Some(Some("human".to_string())));
        assert_eq!(restored.prompt_char_count, Some(Some(128)));
        assert_eq!(restored.attachment_count, Some(Some(2)));
        assert_eq!(AgentMessageValues::event_id() as u16, 6);
    }

    #[test]
    fn test_agent_response_values_roundtrip() {
        use super::PosEncoded;

        let values = AgentResponseValues::new()
            .phase("ended")
            .status("completed")
            .response_char_count(300)
            .inferred(1);
        let sparse = PosEncoded::to_sparse(&values);
        let restored = <AgentResponseValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.phase, Some(Some("ended".to_string())));
        assert_eq!(restored.status, Some(Some("completed".to_string())));
        assert_eq!(restored.response_char_count, Some(Some(300)));
        assert_eq!(restored.is_inferred, Some(Some(1)));
        assert_eq!(AgentResponseValues::event_id() as u16, 7);
    }

    #[test]
    fn test_agent_tool_call_values_roundtrip() {
        use super::PosEncoded;

        let values = AgentToolCallValues::new()
            .phase("failed")
            .tool_name("Write")
            .tool_use_id("call-1")
            .duration_ms(500)
            .failure_type("timeout");
        let sparse = PosEncoded::to_sparse(&values);
        let restored = <AgentToolCallValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.phase, Some(Some("failed".to_string())));
        assert_eq!(restored.tool_name, Some(Some("Write".to_string())));
        assert_eq!(restored.tool_use_id, Some(Some("call-1".to_string())));
        assert_eq!(restored.duration_ms, Some(Some(500)));
        assert_eq!(restored.failure_type, Some(Some("timeout".to_string())));
        assert_eq!(AgentToolCallValues::event_id() as u16, 8);
    }

    #[test]
    fn test_agent_mcp_call_values_roundtrip() {
        use super::PosEncoded;

        let values = AgentMcpCallValues::new()
            .phase("ended")
            .mcp_server("mintmcp")
            .tool_name("mcp__tool")
            .transport("stdio")
            .duration_ms(1200);
        let sparse = PosEncoded::to_sparse(&values);
        let restored = <AgentMcpCallValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.phase, Some(Some("ended".to_string())));
        assert_eq!(restored.mcp_server, Some(Some("mintmcp".to_string())));
        assert_eq!(restored.tool_name, Some(Some("mcp__tool".to_string())));
        assert_eq!(restored.transport, Some(Some("stdio".to_string())));
        assert_eq!(restored.duration_ms, Some(Some(1200)));
        assert_eq!(AgentMcpCallValues::event_id() as u16, 9);
    }

    #[test]
    fn test_agent_skill_usage_values_roundtrip() {
        use super::PosEncoded;

        let values = AgentSkillUsageValues::new()
            .skill_name("security-review")
            .detection_method("inferred_tool")
            .inferred(1);
        let sparse = PosEncoded::to_sparse(&values);
        let restored = <AgentSkillUsageValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(
            restored.skill_name,
            Some(Some("security-review".to_string()))
        );
        assert_eq!(
            restored.detection_method,
            Some(Some("inferred_tool".to_string()))
        );
        assert_eq!(restored.is_inferred, Some(Some(1)));
        assert_eq!(AgentSkillUsageValues::event_id() as u16, 10);
    }

    #[test]
    fn test_agent_subagent_values_roundtrip() {
        use super::PosEncoded;

        let values = AgentSubagentValues::new()
            .phase("ended")
            .subagent_id("sub-1")
            .subagent_type("explore")
            .status("completed")
            .duration_ms(4567)
            .result_char_count(500);
        let sparse = PosEncoded::to_sparse(&values);
        let restored = <AgentSubagentValues as PosEncoded>::from_sparse(&sparse);

        assert_eq!(restored.phase, Some(Some("ended".to_string())));
        assert_eq!(restored.subagent_id, Some(Some("sub-1".to_string())));
        assert_eq!(restored.subagent_type, Some(Some("explore".to_string())));
        assert_eq!(restored.status, Some(Some("completed".to_string())));
        assert_eq!(restored.duration_ms, Some(Some(4567)));
        assert_eq!(restored.result_char_count, Some(Some(500)));
        assert_eq!(AgentSubagentValues::event_id() as u16, 11);
    }
}
