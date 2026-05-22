use rand::RngExt;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::repos::test_repo::TestRepo;

use super::generators::{
    self, CombinedOp, DestructiveOp, EditStrategy, FileOp, PartialStageOp, RewriteOp, StressOp,
};
use super::operations::{
    EditParams, FileState, execute_alternating_amend, execute_alternating_amend_storm,
    execute_amend_attribution_flip, execute_amend_chain, execute_amend_reset_cycle,
    execute_amend_with_deletion, execute_branch_switch_dirty, execute_checkout_discard,
    execute_checkpoint_nonexistent, execute_checkpoint_storm, execute_checkpoint_then_overwrite,
    execute_cherry_pick_conflict, execute_commit, execute_concurrent_file_creation,
    execute_create_delete_batch, execute_cross_file_checkpoint_race, execute_delete_and_recreate,
    execute_discard_then_reedit, execute_double_commit_rapid, execute_edit_and_checkpoint,
    execute_empty_commit_interleave, execute_empty_tree_rebuild, execute_exponential_amend,
    execute_ff_merge, execute_file_rename, execute_fixup_squash, execute_hard_reset,
    execute_interleaved_multi_file, execute_interleaved_partial_commits, execute_mixed_reset,
    execute_move_to_subdir, execute_multi_commit_rebase, execute_multi_squash,
    execute_orphaned_checkpoints, execute_partial_amend_flip, execute_partial_stage_commit,
    execute_partial_then_amend, execute_rapid_branch_merge, execute_rapid_checkpoint_burst,
    execute_rebase_cherry_pick_combo, execute_rebase_same_file, execute_rebase_then_amend,
    execute_recommit_loop, execute_rename_chain, execute_reset_and_reedit,
    execute_reset_edit_recommit, execute_revert_then_redo, execute_selective_file_commit,
    execute_selective_multi_file_commit, execute_session_interleave, execute_soft_reset_recommit,
    execute_squash_partial_stage, execute_squash_same_file, execute_stash_during_work,
    execute_stash_pathspec, execute_stash_pop_cycle, execute_thrash, execute_two_branch_merge,
    execute_whitespace_noise, read_file_state_from_disk,
};
use super::oracle::CharRegistry;

pub struct FuzzerConfig {
    pub seed: u64,
    pub ops: usize,
    pub rewrite_ratio: f64,
    pub destructive_ratio: f64,
    pub partial_stage_ratio: f64,
    pub file_op_ratio: f64,
    pub stress_ratio: f64,
    pub combined_ratio: f64,
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
            rewrite_ratio: 0.12,
            destructive_ratio: 0.12,
            partial_stage_ratio: 0.12,
            file_op_ratio: 0.08,
            stress_ratio: 0.1,
            combined_ratio: 0.12,
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
            rewrite_ratio: 0.45,
            destructive_ratio: 0.08,
            partial_stage_ratio: 0.08,
            file_op_ratio: 0.04,
            stress_ratio: 0.08,
            combined_ratio: 0.1,
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
            rewrite_ratio: 0.05,
            destructive_ratio: 0.05,
            partial_stage_ratio: 0.08,
            file_op_ratio: 0.04,
            stress_ratio: 0.38,
            combined_ratio: 0.1,
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
            destructive_ratio: 0.05,
            partial_stage_ratio: 0.45,
            file_op_ratio: 0.04,
            stress_ratio: 0.08,
            combined_ratio: 0.1,
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
            rewrite_ratio: 0.05,
            destructive_ratio: 0.4,
            partial_stage_ratio: 0.08,
            file_op_ratio: 0.08,
            stress_ratio: 0.08,
            combined_ratio: 0.1,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn file_ops_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.05,
            destructive_ratio: 0.08,
            partial_stage_ratio: 0.08,
            file_op_ratio: 0.4,
            stress_ratio: 0.08,
            combined_ratio: 0.1,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn stress_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.05,
            destructive_ratio: 0.05,
            partial_stage_ratio: 0.05,
            file_op_ratio: 0.05,
            stress_ratio: 0.48,
            combined_ratio: 0.1,
            max_edits_per_commit: 6,
            max_lines_per_edit: 8,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn combined_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.08,
            destructive_ratio: 0.08,
            partial_stage_ratio: 0.08,
            file_op_ratio: 0.05,
            stress_ratio: 0.08,
            combined_ratio: 0.45,
            max_edits_per_commit: 5,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: true,
        }
    }

    pub fn chaos(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.15,
            destructive_ratio: 0.15,
            partial_stage_ratio: 0.15,
            file_op_ratio: 0.12,
            stress_ratio: 0.12,
            combined_ratio: 0.15,
            max_edits_per_commit: 6,
            max_lines_per_edit: 8,
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

    // Extra files created by concurrent creation ops
    let mut extra_files: Vec<FileState> = Vec::new();

    operation_log.push(format!(
        "=== Fuzzer seed={} ops={} rewrite={:.0}% destructive={:.0}% partial={:.0}% file={:.0}% stress={:.0}% combined={:.0}% ===",
        config.seed,
        config.ops,
        config.rewrite_ratio * 100.0,
        config.destructive_ratio * 100.0,
        config.partial_stage_ratio * 100.0,
        config.file_op_ratio * 100.0,
        config.stress_ratio * 100.0,
        config.combined_ratio * 100.0,
    ));

    // Phase 1: Bootstrap
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
        verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
    }

    // Main loop
    let mut completed_ops = 1;
    while completed_ops < config.ops {
        let roll = rng.random_range(0.0..1.0f64);

        let cumulative_rewrite = config.rewrite_ratio;
        let cumulative_destructive = cumulative_rewrite + config.destructive_ratio;
        let cumulative_partial = cumulative_destructive + config.partial_stage_ratio;
        let cumulative_file_op = cumulative_partial + config.file_op_ratio;
        let cumulative_stress = cumulative_file_op + config.stress_ratio;
        let cumulative_combined = cumulative_stress + config.combined_ratio;

        if file_state.lines.len() > 3 && roll < cumulative_rewrite {
            // === REWRITE OPERATIONS ===
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
        } else if file_state.lines.len() > 2 && roll < cumulative_destructive {
            // === DESTRUCTIVE OPERATIONS ===
            let op = generators::gen_destructive_op(&mut rng);
            match op {
                DestructiveOp::HardReset => {
                    execute_hard_reset(&repo, &mut file_state, &mut operation_log);
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
                DestructiveOp::MixedReset => {
                    execute_mixed_reset(
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
                    if !file_state.lines.is_empty() {
                        repo.git(&["add", "-A"]).unwrap();
                        let status = repo.git(&["status", "--porcelain"]).unwrap();
                        if !status.trim().is_empty() {
                            repo.commit("commit after stash pop").unwrap();
                        }
                    }
                }
                DestructiveOp::StashPathspec => {
                    let sec_idx = rng.random_range(0..secondary_files.len());
                    // Ensure secondary file exists on disk
                    if secondary_files[sec_idx].lines.is_empty() {
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
                            &mut secondary_files[sec_idx],
                            &mut registry,
                            &params,
                            &mut rng,
                            &mut operation_log,
                        );
                        repo.git(&["add", "-A"]).unwrap();
                        repo.commit("bootstrap secondary for stash pathspec")
                            .unwrap();
                    }
                    execute_stash_pathspec(
                        &repo,
                        &mut file_state,
                        &mut secondary_files[sec_idx],
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
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
                DestructiveOp::OrphanedCheckpoints => {
                    execute_orphaned_checkpoints(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    // Make a real edit after so we have something meaningful
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
                    execute_commit(
                        &repo,
                        "commit after orphaned checkpoints",
                        &mut operation_log,
                    );
                }
                DestructiveOp::EmptyCommitInterleave => {
                    execute_empty_commit_interleave(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                DestructiveOp::StashDuringWork => {
                    execute_stash_during_work(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
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
            let status = repo.git(&["status", "--porcelain"]).unwrap();
            if status.trim().is_empty() && !file_state.lines.is_empty() {
                verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
            }
        } else if file_state.lines.len() > 1 && roll < cumulative_partial {
            // === PARTIAL STAGING OPERATIONS ===
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
                    registry.verify_blame(
                        &repo,
                        &file_state.filename,
                        &result.committed_lines,
                        &operation_log,
                        config.seed,
                    );
                    if !result.unstaged_lines.is_empty() {
                        repo.git(&["add", "-A"]).unwrap();
                        repo.commit("partial-stage: commit remaining").unwrap();
                        verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                    }
                }
                PartialStageOp::SelectiveFileCommit => {
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
                    let status = repo.git(&["status", "--porcelain"]).unwrap();
                    if !status.trim().is_empty() {
                        repo.git(&["add", "-A"]).unwrap();
                        repo.commit("selective: commit remaining dirty files")
                            .unwrap();
                    }
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
                PartialStageOp::SquashPartialStage => {
                    execute_squash_partial_stage(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
            }
        } else if file_state.lines.len() > 2 && roll < cumulative_file_op {
            // === FILE OPERATIONS ===
            let op = generators::gen_file_op(&mut rng);
            match op {
                FileOp::Rename => {
                    execute_file_rename(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                FileOp::DeleteAndRecreate => {
                    execute_delete_and_recreate(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                FileOp::MoveToSubdir => {
                    execute_move_to_subdir(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                FileOp::ConcurrentCreation => {
                    let new_files = execute_concurrent_file_creation(
                        &repo,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    // Verify all newly created files
                    for nf in &new_files {
                        registry.verify_blame(
                            &repo,
                            &nf.filename,
                            &nf.lines,
                            &operation_log,
                            config.seed,
                        );
                    }
                    extra_files.extend(new_files);
                }
            }
        } else if file_state.lines.len() > 2 && roll < cumulative_stress {
            // === STRESS OPERATIONS ===
            let op = generators::gen_stress_op(&mut rng);
            match op {
                StressOp::RapidCheckpointBurst => {
                    execute_rapid_checkpoint_burst(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::DoubleCommitRapid => {
                    execute_double_commit_rapid(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::AlternatingAmend => {
                    execute_alternating_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::AmendAttributionFlip => {
                    execute_amend_attribution_flip(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::MultiCommitRebase => {
                    execute_multi_commit_rebase(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::Thrash => {
                    execute_thrash(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    let status = repo.git(&["status", "--porcelain"]).unwrap();
                    if status.trim().is_empty() && !file_state.lines.is_empty() {
                        verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                    }
                }
                StressOp::RebaseThenAmend => {
                    execute_rebase_then_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::CheckpointNonexistent => {
                    execute_checkpoint_nonexistent(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::TwoBranchMerge => {
                    execute_two_branch_merge(
                        &repo,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                StressOp::ExponentialAmend => {
                    execute_exponential_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::SessionInterleave => {
                    execute_session_interleave(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::CrossFileCheckpointRace => {
                    let sec_idx = rng.random_range(0..secondary_files.len());
                    let mut main_ref = &mut file_state;
                    let mut sec_ref = &mut secondary_files[sec_idx];
                    execute_cross_file_checkpoint_race(
                        &repo,
                        &mut [&mut main_ref, &mut sec_ref],
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
                StressOp::WhitespaceNoise => {
                    execute_whitespace_noise(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::AmendResetCycle => {
                    execute_amend_reset_cycle(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::PartialThenAmend => {
                    execute_partial_then_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::CheckpointStorm => {
                    execute_checkpoint_storm(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::AlternatingAmendStorm => {
                    execute_alternating_amend_storm(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                StressOp::MultiSquash => {
                    execute_multi_squash(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
            }
        } else if file_state.lines.len() > 2 && roll < cumulative_combined {
            // === COMBINED OPERATIONS ===
            let op = generators::gen_combined_op(&mut rng);
            match op {
                CombinedOp::CherryPickConflict => {
                    execute_cherry_pick_conflict(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::RapidBranchMerge => {
                    execute_rapid_branch_merge(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::RebaseCherryPickCombo => {
                    execute_rebase_cherry_pick_combo(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::ResetEditRecommit => {
                    execute_reset_edit_recommit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::PartialAmendFlip => {
                    execute_partial_amend_flip(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::DiscardThenReedit => {
                    execute_discard_then_reedit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    let status = repo.git(&["status", "--porcelain"]).unwrap();
                    if status.trim().is_empty() && !file_state.lines.is_empty() {
                        verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                    }
                }
                CombinedOp::CreateDeleteBatch => {
                    let kept = execute_create_delete_batch(
                        &repo,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    for f in &kept {
                        registry.verify_blame(
                            &repo,
                            &f.filename,
                            &f.lines,
                            &operation_log,
                            config.seed,
                        );
                    }
                    extra_files.extend(kept);
                }
                CombinedOp::RenameChain => {
                    execute_rename_chain(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::FixupSquash => {
                    execute_fixup_squash(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::EmptyTreeRebuild => {
                    execute_empty_tree_rebuild(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::RevertThenRedo => {
                    execute_revert_then_redo(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    let status = repo.git(&["status", "--porcelain"]).unwrap();
                    if status.trim().is_empty() && !file_state.lines.is_empty() {
                        verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                    }
                }
                CombinedOp::AmendWithDeletion => {
                    execute_amend_with_deletion(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::RecommitLoop => {
                    execute_recommit_loop(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &registry, &file_state, &operation_log, &config);
                }
                CombinedOp::SelectiveMultiFile => {
                    let sec_idx = rng.random_range(0..secondary_files.len());
                    let mut main_ref = &mut file_state;
                    let mut sec_ref = &mut secondary_files[sec_idx];
                    execute_selective_multi_file_commit(
                        &repo,
                        &mut [&mut main_ref, &mut sec_ref],
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
            // === STANDARD MULTI-EDIT COMMIT ===
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
        "[fuzzer] seed={} ops={} chars_allocated={} final_lines={} files_tracked={} -- PASSED",
        config.seed,
        config.ops,
        registry.next_index(),
        file_state.lines.len(),
        1 + secondary_files.len() + extra_files.len(),
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
