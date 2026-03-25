use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FamilyKey(pub String);

impl FamilyKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl std::fmt::Display for FamilyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandScope {
    Family(FamilyKey),
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefChange {
    pub reference: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoContext {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedCommand {
    pub scope: CommandScope,
    pub family_key: Option<FamilyKey>,
    pub worktree: Option<PathBuf>,
    pub root_sid: String,
    pub raw_argv: Vec<String>,
    pub primary_command: Option<String>,
    pub invoked_command: Option<String>,
    pub invoked_args: Vec<String>,
    pub observed_child_commands: Vec<String>,
    pub exit_code: i32,
    pub started_at_ns: u128,
    pub finished_at_ns: u128,
    pub pre_repo: Option<RepoContext>,
    pub post_repo: Option<RepoContext>,
    pub inflight_rebase_original_head: Option<String>,
    pub merge_squash_source_head: Option<String>,
    pub carryover_snapshot_id: Option<String>,
    pub stash_target_oid: Option<String>,
    pub ref_changes: Vec<RefChange>,
    pub confidence: Confidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper_invocation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandClass {
    HistoryRewrite,
    RefMutation,
    WorkspaceMutation,
    Transport,
    RepoAdmin,
    ReadOnly,
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetKind {
    Soft,
    Mixed,
    Hard,
    Merge,
    Keep,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PullStrategy {
    Merge,
    Rebase,
    RebaseMerges,
    FastForwardOnly,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StashOpKind {
    Push,
    Apply,
    Pop,
    Drop,
    List,
    Branch,
    Show,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticEvent {
    CommitCreated {
        base: Option<String>,
        new_head: String,
    },
    CommitAmended {
        old_head: String,
        new_head: String,
    },
    Reset {
        kind: ResetKind,
        old_head: String,
        new_head: String,
    },
    RebaseComplete {
        old_head: String,
        new_head: String,
        interactive: bool,
    },
    RebaseAbort {
        head: String,
    },
    CherryPickComplete {
        original_head: String,
        new_head: String,
    },
    CherryPickAbort {
        head: String,
    },
    MergeSquash {
        base_branch: Option<String>,
        base_head: String,
        source_ref: String,
        source_head: String,
    },
    RefUpdated {
        reference: String,
        old: String,
        new: String,
    },
    BranchCreated {
        name: String,
        target: String,
    },
    BranchDeleted {
        name: String,
        old: String,
    },
    BranchRenamed {
        old_name: String,
        new_name: String,
        target: Option<String>,
    },
    TagCreated {
        name: String,
        target: String,
    },
    TagDeleted {
        name: String,
        old: String,
    },
    SymbolicRefUpdated {
        reference: String,
        old_target: Option<String>,
        new_target: Option<String>,
    },
    NotesUpdated,
    ReplaceUpdated,
    CheckoutPaths,
    RestorePaths,
    CleanedWorkspace,
    StashOperation {
        kind: StashOpKind,
        stash_ref: Option<String>,
        head: Option<String>,
    },
    FetchCompleted {
        remote: Option<String>,
    },
    PullCompleted {
        remote: Option<String>,
        strategy: PullStrategy,
    },
    PushCompleted {
        remote: Option<String>,
    },
    CloneCompleted {
        target: PathBuf,
    },
    LsRemoteCompleted,
    RepoInitialized {
        path: PathBuf,
    },
    WorktreeAdded {
        path: PathBuf,
    },
    WorktreeRemoved {
        path: PathBuf,
    },
    RemoteConfigChanged,
    ConfigChanged,
    MaintenanceRun,
    GcRun,
    PackRefsRun,
    ReflogExpireRun,
    ReadOnlyCommand,
    OpaqueCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub class: CommandClass,
    pub events: Vec<SemanticEvent>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedCommand {
    pub seq: u64,
    pub command: NormalizedCommand,
    pub analysis: AnalysisResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeState {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub last_updated_ns: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyState {
    pub family_key: FamilyKey,
    pub refs: HashMap<String, String>,
    pub worktrees: HashMap<PathBuf, WorktreeState>,
    pub last_error: Option<String>,
    pub applied_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalState {
    pub applied_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyAck {
    pub seq: u64,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyStatus {
    pub family_key: FamilyKey,
    pub applied_seq: u64,
    pub last_error: Option<String>,
}
