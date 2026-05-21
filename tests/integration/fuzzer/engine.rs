use rand::RngExt;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::repos::test_repo::TestRepo;

use super::generators::{self, EditStrategy, RewriteOp};
use super::operations::{
    EditParams, FileState, execute_amend_chain, execute_cherry_pick_same_file, execute_commit,
    execute_edit_and_checkpoint, execute_interleaved_multi_file, execute_rebase_same_file,
    execute_squash_same_file,
};
use super::oracle::CharRegistry;

pub struct FuzzerConfig {
    pub seed: u64,
    pub ops: usize,
    pub rewrite_ratio: f64,
    pub max_edits_per_commit: usize,
    pub max_lines_per_edit: usize,
    pub multi_file_enabled: bool,
    pub allow_destructive: bool,
}

impl FuzzerConfig {
    pub fn standard(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.25,
            max_edits_per_commit: 5,
            max_lines_per_edit: 8,
            multi_file_enabled: true,
            allow_destructive: true,
        }
    }

    pub fn rewrite_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.6,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
        }
    }

    pub fn checkpoint_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.1,
            max_edits_per_commit: 8,
            max_lines_per_edit: 10,
            multi_file_enabled: true,
            allow_destructive: true,
        }
    }
}

pub fn run_fuzzer(config: FuzzerConfig) {
    let mut rng = SmallRng::seed_from_u64(config.seed);
    let repo = TestRepo::new();
    let mut registry = CharRegistry::new();
    let mut operation_log: Vec<String> = Vec::new();
    let mut file_state = FileState::new("fuzz_main.txt");

    // Secondary files for multi-file interleaving
    let mut secondary_files: Vec<FileState> = vec![
        FileState::new("fuzz_secondary_1.txt"),
        FileState::new("fuzz_secondary_2.txt"),
    ];

    operation_log.push(format!(
        "=== Fuzzer seed={} ops={} rewrite_ratio={} max_edits_per_commit={} ===",
        config.seed, config.ops, config.rewrite_ratio, config.max_edits_per_commit
    ));

    // Phase 1: Bootstrap — create file with multiple interleaved edits before first commit
    {
        let edit_count = rng.random_range(2..=config.max_edits_per_commit);
        for _ in 0..edit_count {
            let params = EditParams {
                attribution: generators::gen_attribution(&mut rng),
                strategy: EditStrategy::Append, // Append for bootstrap to guarantee content
                line_count: generators::gen_line_count(&mut rng, config.max_lines_per_edit),
            };
            execute_edit_and_checkpoint(
                &repo,
                &mut file_state,
                &mut registry,
                &params,
                &mut rng,
                &mut operation_log,
            );
        }
        execute_commit(&repo, "initial fuzzer commit", &mut operation_log);
        registry.verify_blame(
            &repo,
            &file_state.filename,
            &file_state.lines,
            &operation_log,
            config.seed,
        );
    }

    // Main loop: ops are either multi-edit-commit cycles or rewrite operations
    let mut completed_ops = 1; // phase 1 counts as 1
    while completed_ops < config.ops {
        let do_rewrite =
            file_state.lines.len() > 3 && rng.random_range(0.0..1.0f64) < config.rewrite_ratio;

        if do_rewrite {
            let op = generators::gen_rewrite_op(&mut rng);
            match op {
                RewriteOp::Amend => {
                    let chain_len = rng.random_range(1..=3);
                    execute_amend_chain(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        chain_len,
                        config.max_lines_per_edit,
                        config.allow_destructive,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                RewriteOp::CherryPick => {
                    execute_cherry_pick_same_file(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_edits_per_commit,
                        config.max_lines_per_edit,
                        config.allow_destructive,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                RewriteOp::Rebase => {
                    execute_rebase_same_file(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_edits_per_commit,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                RewriteOp::SquashMerge => {
                    execute_squash_same_file(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
            }
            registry.verify_blame(
                &repo,
                &file_state.filename,
                &file_state.lines,
                &operation_log,
                config.seed,
            );
        } else {
            // Multi-edit commit: multiple interleaved AI/human/untracked edits, then one commit
            let edit_count = rng.random_range(1..=config.max_edits_per_commit);
            for _ in 0..edit_count {
                let strategy = if config.allow_destructive && file_state.lines.len() > 2 {
                    EditStrategy::random(&mut rng)
                } else if file_state.lines.is_empty() {
                    EditStrategy::Append
                } else {
                    EditStrategy::random_non_destructive(&mut rng)
                };

                let params = EditParams {
                    attribution: generators::gen_attribution(&mut rng),
                    strategy,
                    line_count: generators::gen_line_count(&mut rng, config.max_lines_per_edit),
                };
                execute_edit_and_checkpoint(
                    &repo,
                    &mut file_state,
                    &mut registry,
                    &params,
                    &mut rng,
                    &mut operation_log,
                );
            }

            // Occasionally interleave edits to secondary files (stresses daemon with
            // rapid cross-file checkpoints before the commit)
            if config.multi_file_enabled && rng.random_range(0..100u32) < 30 {
                let sec_idx = rng.random_range(0..secondary_files.len());
                execute_interleaved_multi_file(
                    &repo,
                    &mut secondary_files[sec_idx],
                    &mut registry,
                    config.max_lines_per_edit,
                    &mut rng,
                    &mut operation_log,
                );
            }

            execute_commit(
                &repo,
                &format!("fuzzer commit op {}", completed_ops),
                &mut operation_log,
            );

            registry.verify_blame(
                &repo,
                &file_state.filename,
                &file_state.lines,
                &operation_log,
                config.seed,
            );

            // Also verify secondary files if they have content
            for sec_file in &secondary_files {
                if !sec_file.lines.is_empty() {
                    registry.verify_blame(
                        &repo,
                        &sec_file.filename,
                        &sec_file.lines,
                        &operation_log,
                        config.seed,
                    );
                }
            }
        }

        completed_ops += 1;
    }

    eprintln!(
        "[fuzzer] seed={} ops={} chars_allocated={} final_lines={} -- PASSED",
        config.seed,
        config.ops,
        registry.next_index(),
        file_state.lines.len()
    );
}
