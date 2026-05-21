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
    CherryPick,
    Rebase,
    SquashMerge,
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
/// Distribution: 50% Ai, 30% KnownHuman, 20% Untracked.
pub fn gen_attribution(rng: &mut impl Rng) -> Attribution {
    let roll = rng.random_range(0..100u32);
    if roll < 50 {
        Attribution::Ai
    } else if roll < 80 {
        Attribution::KnownHuman
    } else {
        Attribution::Untracked
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
        1 => RewriteOp::CherryPick,
        2 => RewriteOp::Rebase,
        _ => RewriteOp::SquashMerge,
    }
}
