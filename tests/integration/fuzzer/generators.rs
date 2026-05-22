use rand::Rng;
use rand::RngExt;

use super::oracle::Attribution;

/// Strategies for editing a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditStrategy {
    Append,
    Prepend,
    InsertRandom,
    ReplaceRandom,
    DeleteAndInsert,
    OverwriteAll,
}

/// Rewrite operations that test git history rewriting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteOp {
    Amend,
    FfMerge,
    Rebase,
    SquashMerge,
}

/// Destructive/pathological operations that stress the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestructiveOp {
    HardReset,
    SoftResetRecommit,
    MixedReset,
    CheckoutDiscard,
    StashPop,
    StashPathspec,
    BranchSwitchDirty,
    ResetAndReedit,
    CheckpointOverwrite,
    OrphanedCheckpoints,
    EmptyCommitInterleave,
    StashDuringWork,
}

/// Partial staging strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialStageOp {
    PartialLineStage,
    SelectiveFileCommit,
    InterleavedPartialCommits,
    SquashPartialStage,
}

/// File-system operations (rename, delete, move, concurrent creation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    Rename,
    DeleteAndRecreate,
    MoveToSubdir,
    ConcurrentCreation,
}

/// Stress operations that push the daemon's sequencer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StressOp {
    RapidCheckpointBurst,
    DoubleCommitRapid,
    AlternatingAmend,
    AmendAttributionFlip,
    MultiCommitRebase,
    Thrash,
    RebaseThenAmend,
    CheckpointNonexistent,
    TwoBranchMerge,
    ExponentialAmend,
    SessionInterleave,
    CrossFileCheckpointRace,
    WhitespaceNoise,
    AmendResetCycle,
    PartialThenAmend,
    CheckpointStorm,
    AlternatingAmendStorm,
    MultiSquash,
}

/// Combined/extreme operations that test multiple git features simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombinedOp {
    CherryPickConflict,
    RapidBranchMerge,
    RebaseCherryPickCombo,
    ResetEditRecommit,
    PartialAmendFlip,
    DiscardThenReedit,
    CreateDeleteBatch,
    RenameChain,
    FixupSquash,
    EmptyTreeRebuild,
    RevertThenRedo,
    AmendWithDeletion,
    RecommitLoop,
    SelectiveMultiFile,
    InitialCarryover,
    MergeConflictResolve,
    DoubleCheckpointRace,
    HunkPartialStage,
    RenameDuringEdit,
    NoopOverwrite,
    ConcurrentSessions,
    AmendShrink,
    DeepRebaseChain,
    UntrackedInterleave,
    RapidHeadChange,
    ThreeWayMerge,
    EdgeCaseCommitFlags,
    RapidLifecycle,
    MultiStash,
    OverwriteAndRollback,
    CherryPickChain,
    InterleavedAmendNew,
}

impl EditStrategy {
    /// Generate a random edit strategy.
    pub fn random(rng: &mut impl Rng) -> Self {
        match rng.random_range(0..6u32) {
            0 => EditStrategy::Append,
            1 => EditStrategy::Prepend,
            2 => EditStrategy::InsertRandom,
            3 => EditStrategy::ReplaceRandom,
            4 => EditStrategy::DeleteAndInsert,
            _ => EditStrategy::OverwriteAll,
        }
    }

    /// Generate a non-destructive edit strategy (no OverwriteAll).
    pub fn random_non_destructive(rng: &mut impl Rng) -> Self {
        match rng.random_range(0..5u32) {
            0 => EditStrategy::Append,
            1 => EditStrategy::Prepend,
            2 => EditStrategy::InsertRandom,
            3 => EditStrategy::ReplaceRandom,
            _ => EditStrategy::DeleteAndInsert,
        }
    }
}

/// Generate a random attribution type.
/// Distribution: 60% Ai, 40% KnownHuman.
pub fn gen_attribution(rng: &mut impl Rng) -> Attribution {
    let roll = rng.random_range(0..100u32);
    if roll < 60 {
        Attribution::Ai
    } else {
        Attribution::KnownHuman
    }
}

/// Generate a random line count in range 1..=max.
pub fn gen_line_count(rng: &mut impl Rng, max: usize) -> usize {
    if max == 0 {
        1
    } else {
        rng.random_range(1..=max)
    }
}

/// Generate a random rewrite operation.
pub fn gen_rewrite_op(rng: &mut impl Rng) -> RewriteOp {
    match rng.random_range(0..4u32) {
        0 => RewriteOp::Amend,
        1 => RewriteOp::FfMerge,
        2 => RewriteOp::Rebase,
        _ => RewriteOp::SquashMerge,
    }
}

/// Generate a random destructive operation.
pub fn gen_destructive_op(rng: &mut impl Rng) -> DestructiveOp {
    match rng.random_range(0..12u32) {
        0 => DestructiveOp::HardReset,
        1 => DestructiveOp::SoftResetRecommit,
        2 => DestructiveOp::MixedReset,
        3 => DestructiveOp::CheckoutDiscard,
        4 => DestructiveOp::StashPop,
        5 => DestructiveOp::StashPathspec,
        6 => DestructiveOp::BranchSwitchDirty,
        7 => DestructiveOp::ResetAndReedit,
        8 => DestructiveOp::CheckpointOverwrite,
        9 => DestructiveOp::OrphanedCheckpoints,
        10 => DestructiveOp::EmptyCommitInterleave,
        _ => DestructiveOp::StashDuringWork,
    }
}

/// Generate a random partial staging operation.
pub fn gen_partial_stage_op(rng: &mut impl Rng) -> PartialStageOp {
    match rng.random_range(0..4u32) {
        0 => PartialStageOp::PartialLineStage,
        1 => PartialStageOp::SelectiveFileCommit,
        2 => PartialStageOp::InterleavedPartialCommits,
        _ => PartialStageOp::SquashPartialStage,
    }
}

/// Generate a random file operation.
pub fn gen_file_op(rng: &mut impl Rng) -> FileOp {
    match rng.random_range(0..4u32) {
        0 => FileOp::Rename,
        1 => FileOp::DeleteAndRecreate,
        2 => FileOp::MoveToSubdir,
        _ => FileOp::ConcurrentCreation,
    }
}

/// Generate a random stress operation.
pub fn gen_stress_op(rng: &mut impl Rng) -> StressOp {
    match rng.random_range(0..18u32) {
        0 => StressOp::RapidCheckpointBurst,
        1 => StressOp::DoubleCommitRapid,
        2 => StressOp::AlternatingAmend,
        3 => StressOp::AmendAttributionFlip,
        4 => StressOp::MultiCommitRebase,
        5 => StressOp::Thrash,
        6 => StressOp::RebaseThenAmend,
        7 => StressOp::CheckpointNonexistent,
        8 => StressOp::TwoBranchMerge,
        9 => StressOp::ExponentialAmend,
        10 => StressOp::SessionInterleave,
        11 => StressOp::CrossFileCheckpointRace,
        12 => StressOp::WhitespaceNoise,
        13 => StressOp::AmendResetCycle,
        14 => StressOp::PartialThenAmend,
        15 => StressOp::CheckpointStorm,
        16 => StressOp::AlternatingAmendStorm,
        _ => StressOp::MultiSquash,
    }
}

/// Generate a random combined operation.
pub fn gen_combined_op(rng: &mut impl Rng) -> CombinedOp {
    match rng.random_range(0..32u32) {
        0 => CombinedOp::CherryPickConflict,
        1 => CombinedOp::RapidBranchMerge,
        2 => CombinedOp::RebaseCherryPickCombo,
        3 => CombinedOp::ResetEditRecommit,
        4 => CombinedOp::PartialAmendFlip,
        5 => CombinedOp::DiscardThenReedit,
        6 => CombinedOp::CreateDeleteBatch,
        7 => CombinedOp::RenameChain,
        8 => CombinedOp::FixupSquash,
        9 => CombinedOp::EmptyTreeRebuild,
        10 => CombinedOp::RevertThenRedo,
        11 => CombinedOp::AmendWithDeletion,
        12 => CombinedOp::RecommitLoop,
        13 => CombinedOp::SelectiveMultiFile,
        14 => CombinedOp::InitialCarryover,
        15 => CombinedOp::MergeConflictResolve,
        16 => CombinedOp::DoubleCheckpointRace,
        17 => CombinedOp::HunkPartialStage,
        18 => CombinedOp::RenameDuringEdit,
        19 => CombinedOp::NoopOverwrite,
        20 => CombinedOp::ConcurrentSessions,
        21 => CombinedOp::AmendShrink,
        22 => CombinedOp::DeepRebaseChain,
        23 => CombinedOp::UntrackedInterleave,
        24 => CombinedOp::RapidHeadChange,
        25 => CombinedOp::ThreeWayMerge,
        26 => CombinedOp::EdgeCaseCommitFlags,
        27 => CombinedOp::RapidLifecycle,
        28 => CombinedOp::MultiStash,
        29 => CombinedOp::OverwriteAndRollback,
        30 => CombinedOp::CherryPickChain,
        _ => CombinedOp::InterleavedAmendNew,
    }
}
