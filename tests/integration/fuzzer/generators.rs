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
    CheckoutDiscard,
    StashPop,
    BranchSwitchDirty,
    ResetAndReedit,
    CheckpointOverwrite,
}

/// Partial staging strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialStageOp {
    PartialLineStage,
    SelectiveFileCommit,
    InterleavedPartialCommits,
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
    match rng.random_range(0..7u32) {
        0 => DestructiveOp::HardReset,
        1 => DestructiveOp::SoftResetRecommit,
        2 => DestructiveOp::CheckoutDiscard,
        3 => DestructiveOp::StashPop,
        4 => DestructiveOp::BranchSwitchDirty,
        5 => DestructiveOp::ResetAndReedit,
        _ => DestructiveOp::CheckpointOverwrite,
    }
}

/// Generate a random partial staging operation.
pub fn gen_partial_stage_op(rng: &mut impl Rng) -> PartialStageOp {
    match rng.random_range(0..3u32) {
        0 => PartialStageOp::PartialLineStage,
        1 => PartialStageOp::SelectiveFileCommit,
        _ => PartialStageOp::InterleavedPartialCommits,
    }
}
