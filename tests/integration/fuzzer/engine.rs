use rand::RngExt;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::repos::test_repo::TestRepo;

use super::generators::{EditStrategy, RewriteOp, gen_attribution, gen_line_count, gen_rewrite_op};
use super::operations::{
    EditParams, FileState, execute_amend, execute_cherry_pick, execute_commit,
    execute_edit_and_checkpoint, execute_rebase, execute_squash_merge,
};
use super::oracle::CharRegistry;

/// Configuration for a fuzzer run.
pub struct FuzzerConfig {
    pub seed: u64,
    pub ops: usize,
    /// Ratio of rewrite ops vs normal edits (0.0 to 1.0).
    pub rewrite_ratio: f64,
}

impl FuzzerConfig {
    /// Standard fuzzer: balanced mix of edits and occasional rewrites.
    pub fn standard(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.15,
        }
    }

    /// Rewrite-heavy: more amend/cherry-pick/rebase/squash operations.
    pub fn rewrite_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.5,
        }
    }

    /// Checkpoint-heavy: lots of edits with frequent commits to stress checkpoint logic.
    pub fn checkpoint_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.05,
        }
    }
}

/// Run the fuzzer with the given configuration.
///
/// Each operation is one edit+commit cycle. This ensures clean attribution
/// boundaries: each commit has exactly one attribution type (AI, KnownHuman,
/// or Untracked), matching how real usage works (one AI session per commit
/// or one human editing session per commit).
pub fn run_fuzzer(config: FuzzerConfig) {
    let mut rng = SmallRng::seed_from_u64(config.seed);
    let repo = TestRepo::new();
    let mut registry = CharRegistry::new();
    let mut operation_log: Vec<String> = Vec::new();
    let mut file_state = FileState::new("fuzz_main.txt");

    operation_log.push(format!(
        "=== Fuzzer seed={} ops={} ===",
        config.seed, config.ops
    ));

    // Phase 1: Initial edit + commit + verify
    {
        let params = EditParams {
            attribution: gen_attribution(&mut rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(&mut rng, 5),
        };

        execute_edit_and_checkpoint(
            &repo,
            &mut file_state,
            &mut registry,
            &params,
            &mut rng,
            &mut operation_log,
        );

        execute_commit(&repo, "initial fuzzer commit", &mut operation_log);

        registry.verify_blame(
            &repo,
            &file_state.filename,
            &file_state.lines,
            &operation_log,
            config.seed,
        );
    }

    // Phase 2: Linear edit+commit cycles
    let phase2_ops = (config.ops as f64 * 0.6) as usize;
    let phase3_ops = config.ops - phase2_ops - 1; // -1 for phase 1

    for i in 0..phase2_ops {
        let params = EditParams {
            attribution: gen_attribution(&mut rng),
            strategy: EditStrategy::random_non_destructive(&mut rng),
            line_count: gen_line_count(&mut rng, 4),
        };

        execute_edit_and_checkpoint(
            &repo,
            &mut file_state,
            &mut registry,
            &params,
            &mut rng,
            &mut operation_log,
        );

        execute_commit(
            &repo,
            &format!("fuzzer commit phase2 op {}", i),
            &mut operation_log,
        );

        registry.verify_blame(
            &repo,
            &file_state.filename,
            &file_state.lines,
            &operation_log,
            config.seed,
        );
    }

    // Phase 3: Rewrite operations mixed with normal edits
    for i in 0..phase3_ops {
        let do_rewrite = rng.random_range(0.0..1.0f64) < config.rewrite_ratio;

        if do_rewrite {
            let op = gen_rewrite_op(&mut rng);
            match op {
                RewriteOp::Amend => {
                    execute_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        &mut rng,
                        &mut operation_log,
                    );

                    registry.verify_blame(
                        &repo,
                        &file_state.filename,
                        &file_state.lines,
                        &operation_log,
                        config.seed,
                    );
                }
                RewriteOp::CherryPick => {
                    execute_cherry_pick(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        &mut rng,
                        &mut operation_log,
                    );

                    registry.verify_blame(
                        &repo,
                        &file_state.filename,
                        &file_state.lines,
                        &operation_log,
                        config.seed,
                    );
                }
                RewriteOp::Rebase => {
                    execute_rebase(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        &mut rng,
                        &mut operation_log,
                    );
                    // Rebase uses separate files, main file_state unchanged
                    registry.verify_blame(
                        &repo,
                        &file_state.filename,
                        &file_state.lines,
                        &operation_log,
                        config.seed,
                    );
                }
                RewriteOp::SquashMerge => {
                    execute_squash_merge(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        &mut rng,
                        &mut operation_log,
                    );
                    // Squash merge uses separate files, main file_state unchanged
                    registry.verify_blame(
                        &repo,
                        &file_state.filename,
                        &file_state.lines,
                        &operation_log,
                        config.seed,
                    );
                }
            }
        } else {
            // Normal edit + commit
            let params = EditParams {
                attribution: gen_attribution(&mut rng),
                strategy: EditStrategy::random_non_destructive(&mut rng),
                line_count: gen_line_count(&mut rng, 3),
            };

            execute_edit_and_checkpoint(
                &repo,
                &mut file_state,
                &mut registry,
                &params,
                &mut rng,
                &mut operation_log,
            );

            execute_commit(
                &repo,
                &format!("fuzzer commit phase3 op {}", i),
                &mut operation_log,
            );

            registry.verify_blame(
                &repo,
                &file_state.filename,
                &file_state.lines,
                &operation_log,
                config.seed,
            );
        }
    }

    eprintln!(
        "[fuzzer] seed={} ops={} chars_allocated={} -- PASSED",
        config.seed,
        config.ops,
        registry.next_index()
    );
}
