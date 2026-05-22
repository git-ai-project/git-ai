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
    match rng.random_range(0..11u32) {
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
        _ => DestructiveOp::EmptyCommitInterleave,
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
    match rng.random_range(0..11u32) {
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
        _ => StressOp::SessionInterleave,
    }
}
