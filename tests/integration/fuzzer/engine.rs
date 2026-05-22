use rand::RngExt;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::repos::test_repo::TestRepo;

use super::generators::{self, DestructiveOp, EditStrategy, PartialStageOp, RewriteOp};
use super::operations::{
    EditParams, FileState, execute_amend_chain, execute_branch_switch_dirty,
    execute_checkout_discard, execute_checkpoint_then_overwrite, execute_commit,
    execute_edit_and_checkpoint, execute_ff_merge, execute_hard_reset,
    execute_interleaved_multi_file, execute_interleaved_partial_commits,
    execute_partial_stage_commit, execute_rebase_same_file, execute_reset_and_reedit,
    execute_selective_file_commit, execute_soft_reset_recommit, execute_squash_same_file,
    execute_stash_pop_cycle, read_file_state_from_disk,
};
use super::oracle::CharRegistry;

pub struct FuzzerConfig {
    pub seed: u64,
    pub ops: usize,
    pub rewrite_ratio: f64,
    pub destructive_ratio: f64,
    pub partial_stage_ratio: f64,
    pub max_edits_per_commit: usize,
    pub max_lines_per_edit: usize,
    pub multi_file_enabled: bool,
    pub allow_destructive: bool,
    pub verify_sessions: bool,
}

impl FuzzerConfig {
    pub fn standard(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.25,
            destructive_ratio: 0.15,
            partial_stage_ratio: 0.2,
            max_edits_per_commit: 5,
            max_lines_per_edit: 8,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn rewrite_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.6,
            destructive_ratio: 0.1,
            partial_stage_ratio: 0.1,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn checkpoint_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.1,
            destructive_ratio: 0.1,
            partial_stage_ratio: 0.15,
            max_edits_per_commit: 8,
            max_lines_per_edit: 10,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn partial_stage_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.05,
            destructive_ratio: 0.1,
            partial_stage_ratio: 0.6,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn destructive_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.1,
            destructive_ratio: 0.5,
            partial_stage_ratio: 0.1,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }
}

pub fn run_fuzzer(config: FuzzerConfig) {
    let mut rng = SmallRng::seed_from_u64(config.seed);
    let repo = TestRepo::new();
    let mut registry = CharRegistry::new();
    let mut operation_log: Vec<String> = Vec::new();
    let mut file_state = FileState::new("fuzz_main.txt");

    // Secondary files for multi-file interleaving and partial staging
    let mut secondary_files: Vec<FileState> = vec![
        FileState::new("fuzz_secondary_1.txt"),
        FileState::new("fuzz_secondary_2.txt"),
        FileState::new("fuzz_secondary_3.txt"),
    ];

    operation_log.push(format!(
        "=== Fuzzer seed={} ops={} rewrite={:.0}% destructive={:.0}% partial_stage={:.0}% ===",
        config.seed,
        config.ops,
        config.rewrite_ratio * 100.0,
        config.destructive_ratio * 100.0,
        config.partial_stage_ratio * 100.0,
    ));

    // Phase 1: Bootstrap — create file with multiple interleaved edits before first commit
    {
        let edit_count = rng.random_range(2..=config.max_edits_per_commit);
        for _ in 0..edit_count {
            let params = EditParams {
                attribution: generators::gen_attribution(&mut rng),
                strategy: EditStrategy::Append,
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
        if config.verify_sessions {
            registry.verify_sessions(&repo, &file_state.lines, &operation_log, config.seed);
        }
    }

    // Main loop
    let mut completed_ops = 1;
    while completed_ops < config.ops {
        let roll = rng.random_range(0.0..1.0f64);

        // Decide what kind of operation to do
        if file_state.lines.len() > 3 && roll < config.rewrite_ratio {
            // Rewrite operation
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
                RewriteOp::FfMerge => {
                    execute_ff_merge(
                        &repo,
                        &mut registry,
                        config.max_edits_per_commit,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
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
            verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
        } else if file_state.lines.len() > 2
            && roll < config.rewrite_ratio + config.destructive_ratio
        {
            // Destructive/pathological operation
            let op = generators::gen_destructive_op(&mut rng);
            match op {
                DestructiveOp::HardReset => {
                    execute_hard_reset(&repo, &mut file_state, &mut operation_log);
                    // After hard reset, we need to re-bootstrap if file is empty
                    if file_state.lines.is_empty() {
                        let params = EditParams {
                            attribution: generators::gen_attribution(&mut rng),
                            strategy: EditStrategy::Append,
                            line_count: generators::gen_line_count(
                                &mut rng,
                                config.max_lines_per_edit,
                            ),
                        };
                        execute_edit_and_checkpoint(
                            &repo,
                            &mut file_state,
                            &mut registry,
                            &params,
                            &mut rng,
                            &mut operation_log,
                        );
                        execute_commit(&repo, "re-bootstrap after reset", &mut operation_log);
                    }
                }
                DestructiveOp::SoftResetRecommit => {
                    execute_soft_reset_recommit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                DestructiveOp::CheckoutDiscard => {
                    execute_checkout_discard(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    // After discard, make a real edit and commit so we have something to verify
                    let params = EditParams {
                        attribution: generators::gen_attribution(&mut rng),
                        strategy: EditStrategy::Append,
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
                    execute_commit(&repo, "commit after checkout discard", &mut operation_log);
                }
                DestructiveOp::StashPop => {
                    execute_stash_pop_cycle(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    // Commit the popped changes
                    if !file_state.lines.is_empty() {
                        repo.git(&["add", "-A"]).unwrap();
                        let status = repo.git(&["status", "--porcelain"]).unwrap();
                        if !status.trim().is_empty() {
                            repo.commit("commit after stash pop").unwrap();
                        }
                    }
                }
                DestructiveOp::BranchSwitchDirty => {
                    execute_branch_switch_dirty(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                DestructiveOp::ResetAndReedit => {
                    execute_reset_and_reedit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                DestructiveOp::CheckpointOverwrite => {
                    execute_checkpoint_then_overwrite(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    execute_commit(
                        &repo,
                        "commit after checkpoint overwrite",
                        &mut operation_log,
                    );
                }
            }
            // Re-sync file state from disk after destructive ops
            let actual = read_file_state_from_disk(&repo, &file_state.filename);
            if actual != file_state.lines {
                operation_log.push(format!(
                    "post-destructive: model had {} lines, disk has {}, trusting disk",
                    file_state.lines.len(),
                    actual.len()
                ));
                file_state.lines = actual;
            }
            // Only verify blame if we have a clean committed state
            let status = repo.git(&["status", "--porcelain"]).unwrap();
            if status.trim().is_empty() && !file_state.lines.is_empty() {
                verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
            }
        } else if file_state.lines.len() > 1
            && roll < config.rewrite_ratio + config.destructive_ratio + config.partial_stage_ratio
        {
            // Partial staging operation
            let op = generators::gen_partial_stage_op(&mut rng);
            match op {
                PartialStageOp::PartialLineStage => {
                    let result = execute_partial_stage_commit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    // After partial stage, the committed state has fewer lines than working tree
                    // Verify blame against the committed state (HEAD)
                    registry.verify_blame(
                        &repo,
                        &file_state.filename,
                        &result.committed_lines,
                        &operation_log,
                        config.seed,
                    );
                    // If there are unstaged lines, commit them too
                    if !result.unstaged_lines.is_empty() {
                        repo.git(&["add", "-A"]).unwrap();
                        repo.commit("partial-stage: commit remaining").unwrap();
                        verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                    }
                }
                PartialStageOp::SelectiveFileCommit => {
                    // Need at least one secondary file with content
                    let sec_idx = rng.random_range(0..secondary_files.len());
                    let mut main_ref = &mut file_state;
                    let mut sec_ref = &mut secondary_files[sec_idx];
                    execute_selective_file_commit(
                        &repo,
                        &mut [&mut main_ref, &mut sec_ref],
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    // Commit remaining dirty files
                    let status = repo.git(&["status", "--porcelain"]).unwrap();
                    if !status.trim().is_empty() {
                        repo.git(&["add", "-A"]).unwrap();
                        repo.commit("selective: commit remaining dirty files")
                            .unwrap();
                    }
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                    // Verify secondary too
                    if !secondary_files[sec_idx].lines.is_empty() {
                        registry.verify_blame(
                            &repo,
                            &secondary_files[sec_idx].filename,
                            &secondary_files[sec_idx].lines,
                            &operation_log,
                            config.seed,
                        );
                    }
                }
                PartialStageOp::InterleavedPartialCommits => {
                    let sec_idx = rng.random_range(0..secondary_files.len());
                    execute_interleaved_partial_commits(
                        &repo,
                        &mut file_state,
                        &mut secondary_files[sec_idx],
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                    if !secondary_files[sec_idx].lines.is_empty() {
                        registry.verify_blame(
                            &repo,
                            &secondary_files[sec_idx].filename,
                            &secondary_files[sec_idx].lines,
                            &operation_log,
                            config.seed,
                        );
                    }
                }
            }
        } else {
            // Standard multi-edit commit
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

            // Occasionally interleave edits to secondary files
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

            verify_main_file(&repo, &registry, &file_state, &operation_log, &config);

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

fn verify_main_file(
    repo: &TestRepo,
    registry: &CharRegistry,
    file_state: &FileState,
    operation_log: &[String],
    config: &FuzzerConfig,
) {
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        config.seed,
    );
    if config.verify_sessions {
        registry.verify_sessions(repo, &file_state.lines, operation_log, config.seed);
    }
}
