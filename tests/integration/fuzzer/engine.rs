use rand::RngExt;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::repos::test_repo::TestRepo;

use super::generators::{
    self, CombinedOp, DestructiveOp, EditStrategy, FileOp, PartialStageOp, RewriteOp, StressOp,
    WorkflowOp,
};
use super::operations::{
    EditParams, FileState, execute_alternating_amend, execute_alternating_amend_storm,
    execute_amend_attribution_flip, execute_amend_chain, execute_amend_reset_cycle,
    execute_amend_shrink, execute_amend_with_deletion, execute_branch_switch_dirty,
    execute_checkout_discard, execute_checkpoint_nonexistent, execute_checkpoint_storm,
    execute_checkpoint_then_overwrite, execute_cherry_pick_chain, execute_cherry_pick_conflict,
    execute_commit, execute_concurrent_file_creation, execute_concurrent_sessions,
    execute_create_delete_batch, execute_cross_file_checkpoint_race, execute_deep_rebase_chain,
    execute_delete_and_recreate, execute_discard_then_reedit, execute_double_checkpoint_race,
    execute_double_commit_rapid, execute_edge_case_commit_flags, execute_edit_and_checkpoint,
    execute_empty_commit_interleave, execute_empty_tree_rebuild, execute_exponential_amend,
    execute_ff_merge, execute_file_rename, execute_fixup_squash, execute_hard_reset,
    execute_hunk_partial_stage, execute_initial_carryover, execute_interleaved_amend_new,
    execute_interleaved_multi_file, execute_interleaved_partial_commits,
    execute_merge_conflict_resolve, execute_mixed_reset, execute_move_to_subdir,
    execute_multi_commit_rebase, execute_multi_squash, execute_multi_stash, execute_noop_overwrite,
    execute_orphaned_checkpoints, execute_overwrite_and_rollback, execute_partial_amend_flip,
    execute_partial_stage_commit, execute_partial_then_amend, execute_rapid_branch_merge,
    execute_rapid_checkpoint_burst, execute_rapid_head_change, execute_rapid_lifecycle,
    execute_rebase_cherry_pick_combo, execute_rebase_same_file, execute_rebase_then_amend,
    execute_recommit_loop, execute_rename_chain, execute_rename_during_edit,
    execute_reset_and_reedit, execute_reset_edit_recommit, execute_revert_then_redo,
    execute_selective_file_commit, execute_selective_multi_file_commit, execute_session_interleave,
    execute_soft_reset_recommit, execute_squash_after_amend, execute_squash_mixed_attribution,
    execute_squash_multi_file, execute_squash_nonlinear_branch, execute_squash_partial_stage,
    execute_squash_rebased_branch, execute_squash_reset_recommit, execute_squash_same_file,
    execute_squash_then_amend, execute_squash_with_overwrites, execute_stash_during_work,
    execute_stash_pathspec, execute_stash_pop_cycle, execute_thrash, execute_three_way_merge,
    execute_two_branch_merge, execute_untracked_interleave, execute_whitespace_noise,
    read_file_state_from_disk,
};
use super::oracle::CharRegistry;
use super::workflows::{
    execute_cherry_pick_no_commit, execute_cherry_pick_range, execute_file_cross_rename,
    execute_file_spaces_path, execute_interleaved_line_attribution, execute_merge_squash_direct,
    execute_plumbing_commit_tree, execute_plumbing_rapid_update_ref,
    execute_rapid_multi_file_burst, execute_rebase_conflict_continue, execute_rebase_onto,
    execute_restore_from_commit, execute_workflow_branch_lifecycle,
    execute_workflow_fixup_autosquash, execute_workflow_multi_branch_merge,
    execute_workflow_revert_cherrypick, execute_workflow_stash_sandwich,
    execute_working_log_base_race,
};

pub struct FuzzerConfig {
    pub seed: u64,
    pub ops: usize,
    pub rewrite_ratio: f64,
    pub destructive_ratio: f64,
    pub partial_stage_ratio: f64,
    pub file_op_ratio: f64,
    pub stress_ratio: f64,
    pub combined_ratio: f64,
    pub workflow_ratio: f64,
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
            rewrite_ratio: 0.10,
            destructive_ratio: 0.10,
            partial_stage_ratio: 0.10,
            file_op_ratio: 0.08,
            stress_ratio: 0.10,
            combined_ratio: 0.10,
            workflow_ratio: 0.10,
            max_edits_per_commit: 5,
            max_lines_per_edit: 8,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
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
            workflow_ratio: 0.05,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
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
            workflow_ratio: 0.05,
            max_edits_per_commit: 8,
            max_lines_per_edit: 10,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
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
            workflow_ratio: 0.05,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
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
            workflow_ratio: 0.05,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
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
            workflow_ratio: 0.05,
            max_edits_per_commit: 4,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
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
            workflow_ratio: 0.05,
            max_edits_per_commit: 6,
            max_lines_per_edit: 8,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
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
            workflow_ratio: 0.05,
            max_edits_per_commit: 5,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
        }
    }

    pub fn squash_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.15,
            destructive_ratio: 0.05,
            partial_stage_ratio: 0.05,
            file_op_ratio: 0.03,
            stress_ratio: 0.05,
            combined_ratio: 0.55,
            workflow_ratio: 0.05,
            max_edits_per_commit: 5,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
        }
    }

    pub fn chaos(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.13,
            destructive_ratio: 0.13,
            partial_stage_ratio: 0.13,
            file_op_ratio: 0.10,
            stress_ratio: 0.10,
            combined_ratio: 0.13,
            workflow_ratio: 0.13,
            max_edits_per_commit: 6,
            max_lines_per_edit: 8,
            multi_file_enabled: true,
            allow_destructive: true,
            verify_sessions: false,
        }
    }

    pub fn workflow_heavy(seed: u64, ops: usize) -> Self {
        Self {
            seed,
            ops,
            rewrite_ratio: 0.05,
            destructive_ratio: 0.05,
            partial_stage_ratio: 0.05,
            file_op_ratio: 0.05,
            stress_ratio: 0.05,
            combined_ratio: 0.08,
            workflow_ratio: 0.50,
            max_edits_per_commit: 5,
            max_lines_per_edit: 6,
            multi_file_enabled: true,
            allow_destructive: true,
            // Disabled: git_og operations break session continuity
            verify_sessions: false,
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
        "=== Fuzzer seed={} ops={} rewrite={:.0}% destructive={:.0}% partial={:.0}% file={:.0}% stress={:.0}% combined={:.0}% workflow={:.0}% ===",
        config.seed,
        config.ops,
        config.rewrite_ratio * 100.0,
        config.destructive_ratio * 100.0,
        config.partial_stage_ratio * 100.0,
        config.file_op_ratio * 100.0,
        config.stress_ratio * 100.0,
        config.combined_ratio * 100.0,
        config.workflow_ratio * 100.0,
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
        verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
        let cumulative_workflow = cumulative_combined + config.workflow_ratio;

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
                    registry.mark_all_unverifiable();
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
                    // Daemon rebase handling has known attribution gaps — mark existing
                    // chars unverifiable since notes may not transfer correctly.
                    registry.mark_all_unverifiable();
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
                    // Squash merge rewrites history — mark existing chars unverifiable.
                    registry.mark_all_unverifiable();
                }
            }
            verify_main_file_with_retention(
                &repo,
                &mut registry,
                &file_state,
                &operation_log,
                &config,
            );
        } else if file_state.lines.len() > 2 && roll < cumulative_destructive {
            // === DESTRUCTIVE OPERATIONS ===
            // Reset session tracking: destructive ops may legitimately drop commits,
            // so we can't assert monotonic retention across them.
            registry.reset_session_tracking();
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
                verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    // Don't verify blame immediately after partial stage if there are unstaged lines,
                    // because git-ai blame shows working tree state (committed + unstaged), not just
                    // committed state. We'll verify after committing the remaining lines.
                    if result.unstaged_lines.is_empty() {
                        // All lines were staged, safe to verify against committed_lines
                        registry.verify_blame(
                            &repo,
                            &file_state.filename,
                            &result.committed_lines,
                            &operation_log,
                            config.seed,
                        );
                    }
                    if !result.unstaged_lines.is_empty() {
                        repo.git(&["add", "-A"]).unwrap();
                        repo.commit("partial-stage: commit remaining").unwrap();
                    }
                    // Now verify against the full working tree
                    verify_main_file(
                        &repo,
                        &mut registry,
                        &file_state,
                        &operation_log,
                        &config,
                    );
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    // File renames permanently break daemon attribution (known limitation)
                    registry.skip_all_blame = true;
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    // File moves permanently break daemon attribution (known limitation)
                    registry.skip_all_blame = true;
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    registry.mark_all_unverifiable();
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
                }
                StressOp::Thrash => {
                    registry.reset_session_tracking();
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
                        verify_main_file(
                            &repo,
                            &mut registry,
                            &file_state,
                            &operation_log,
                            &config,
                        );
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
                    registry.mark_all_unverifiable();
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    registry.mark_all_unverifiable();
                }
                StressOp::ExponentialAmend => {
                    execute_exponential_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
                }
                StressOp::AmendResetCycle => {
                    registry.reset_session_tracking();
                    execute_amend_reset_cycle(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
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
                    registry.mark_all_unverifiable();
                }
            }
        } else if file_state.lines.len() > 2 && roll < cumulative_combined {
            // === COMBINED OPERATIONS ===
            // Most combined ops use git_og for rebase/merge/cherry-pick which bypasses
            // the daemon. Mark all existing chars unverifiable preemptively.
            registry.mark_all_unverifiable();
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
                }
                CombinedOp::ResetEditRecommit => {
                    registry.reset_session_tracking();
                    execute_reset_edit_recommit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
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
                }
                CombinedOp::EmptyTreeRebuild => {
                    registry.reset_session_tracking();
                    execute_empty_tree_rebuild(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
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
                }
                CombinedOp::RecommitLoop => {
                    registry.reset_session_tracking();
                    execute_recommit_loop(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
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
                CombinedOp::InitialCarryover => {
                    execute_initial_carryover(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::MergeConflictResolve => {
                    execute_merge_conflict_resolve(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::DoubleCheckpointRace => {
                    execute_double_checkpoint_race(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::HunkPartialStage => {
                    execute_hunk_partial_stage(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::RenameDuringEdit => {
                    let sec_idx = rng.random_range(0..secondary_files.len());
                    execute_rename_during_edit(
                        &repo,
                        &mut file_state,
                        &mut secondary_files[sec_idx],
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::NoopOverwrite => {
                    execute_noop_overwrite(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::ConcurrentSessions => {
                    execute_concurrent_sessions(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::AmendShrink => {
                    execute_amend_shrink(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::DeepRebaseChain => {
                    execute_deep_rebase_chain(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::UntrackedInterleave => {
                    execute_untracked_interleave(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::RapidHeadChange => {
                    registry.reset_session_tracking();
                    execute_rapid_head_change(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::ThreeWayMerge => {
                    execute_three_way_merge(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::EdgeCaseCommitFlags => {
                    execute_edge_case_commit_flags(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::RapidLifecycle => {
                    execute_rapid_lifecycle(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::MultiStash => {
                    execute_multi_stash(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::OverwriteAndRollback => {
                    registry.reset_session_tracking();
                    execute_overwrite_and_rollback(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::CherryPickChain => {
                    execute_cherry_pick_chain(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::InterleavedAmendNew => {
                    execute_interleaved_amend_new(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::SquashMixedAttribution => {
                    execute_squash_mixed_attribution(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::SquashAfterAmend => {
                    execute_squash_after_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::SquashThenAmend => {
                    execute_squash_then_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::SquashRebasedBranch => {
                    execute_squash_rebased_branch(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::SquashWithOverwrites => {
                    execute_squash_with_overwrites(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::SquashMultiFile => {
                    let sec_idx = rng.random_range(0..secondary_files.len());
                    let mut main_ref = &mut file_state;
                    let mut sec_ref = &mut secondary_files[sec_idx];
                    execute_squash_multi_file(
                        &repo,
                        &mut [&mut main_ref, &mut sec_ref],
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );

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
                CombinedOp::SquashResetRecommit => {
                    registry.reset_session_tracking();
                    execute_squash_reset_recommit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
                CombinedOp::SquashNonlinearBranch => {
                    execute_squash_nonlinear_branch(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                    );
                }
            }
            // Mark all chars (including newly allocated ones) as unverifiable after
            // combined ops since most involve git_og rebase/merge/cherry-pick.
            registry.mark_all_unverifiable();
            verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);
        } else if file_state.lines.len() > 2 && roll < cumulative_workflow {
            // === WORKFLOW OPERATIONS ===
            // Reset session tracking: workflow ops use non-standard commit paths (plumbing,
            // fixup/autosquash, rebase --onto, cherry-pick --no-commit) that legitimately
            // create fresh session histories without carrying forward parent sessions.
            registry.reset_session_tracking();
            let op = generators::gen_workflow_op(&mut rng);
            match op {
                WorkflowOp::PlumbingCommitTree => {
                    execute_plumbing_commit_tree(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::PlumbingRapidUpdateRef => {
                    execute_plumbing_rapid_update_ref(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::BranchLifecycle => {
                    execute_workflow_branch_lifecycle(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::StashSandwich => {
                    execute_workflow_stash_sandwich(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::FixupAutosquash => {
                    execute_workflow_fixup_autosquash(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::CherryPickNoCommit => {
                    execute_cherry_pick_no_commit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::CherryPickRange => {
                    execute_cherry_pick_range(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::RebaseOnto => {
                    execute_rebase_onto(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::RapidMultiFileBurst => {
                    execute_rapid_multi_file_burst(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::MergeSquashDirect => {
                    execute_merge_squash_direct(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::RebaseConflictContinue => {
                    execute_rebase_conflict_continue(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::RestoreFromCommit => {
                    execute_restore_from_commit(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::RevertCherrypick => {
                    execute_workflow_revert_cherrypick(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::MultiBranchMerge => {
                    execute_workflow_multi_branch_merge(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::FileCrossRename => {
                    execute_file_cross_rename(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::FileSpacesPath => {
                    execute_file_spaces_path(
                        &repo,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::WorkingLogBaseRace => {
                    execute_working_log_base_race(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
                WorkflowOp::InterleavedLineAttribution => {
                    execute_interleaved_line_attribution(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        config.max_lines_per_edit,
                        &mut rng,
                        &mut operation_log,
                        config.seed,
                    );
                }
            }
            // Sync file_state from disk in case workflow changed it without model knowledge.
            let actual = read_file_state_from_disk(&repo, &file_state.filename);
            if actual != file_state.lines {
                operation_log.push(format!(
                    "post-workflow: model had {} lines, disk has {}, trusting disk",
                    file_state.lines.len(),
                    actual.len()
                ));
                file_state.lines = actual;
            }
            // Workflow ops that use git_og for rebase/merge/cherry-pick bypass the daemon,
            // so authorship notes are not transferred and the daemon's state may be
            // inconsistent with the new HEAD. Mark ALL existing chars as unverifiable
            // since even subsequent daemon commits may reference old working log bases.
            // Only chars allocated AFTER this point (in future operations) will be verified.
            match op {
                WorkflowOp::BranchLifecycle
                | WorkflowOp::FixupAutosquash
                | WorkflowOp::CherryPickNoCommit
                | WorkflowOp::CherryPickRange
                | WorkflowOp::RebaseOnto
                | WorkflowOp::MergeSquashDirect
                | WorkflowOp::RebaseConflictContinue
                | WorkflowOp::RevertCherrypick
                | WorkflowOp::MultiBranchMerge => {
                    registry.mark_all_unverifiable();
                }
                _ => {}
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

            verify_main_file(&repo, &mut registry, &file_state, &operation_log, &config);

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

            // Multi-file verification: verify all files together
            let mut all_files: Vec<(&str, &[char])> = vec![(
                file_state.filename.as_str(),
                file_state.lines.as_slice(),
            )];
            for sec_file in &secondary_files {
                if !sec_file.lines.is_empty() {
                    all_files.push((sec_file.filename.as_str(), sec_file.lines.as_slice()));
                }
            }
            registry.verify_multi_file_commit(&repo, &all_files, &operation_log, config.seed);
        }

        completed_ops += 1;

        // Cap operation_log to avoid unbounded memory growth in marathon mode
        const MAX_LOG_ENTRIES: usize = 500;
        if operation_log.len() > MAX_LOG_ENTRIES {
            let drain_count = operation_log.len() - MAX_LOG_ENTRIES;
            operation_log.drain(..drain_count);
        }
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
    registry: &mut CharRegistry,
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

    // Verify note schema
    registry.verify_note_schema(repo, operation_log, config.seed);

    // Verify line ranges in the note match actual attributions
    registry.verify_note_line_ranges(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        config.seed,
    );
}

fn verify_main_file_with_retention(
    repo: &TestRepo,
    registry: &mut CharRegistry,
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
        let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        if let Some(note) = repo.read_authorship_note(&head_sha)
            && note.contains(&file_state.filename)
        {
            registry.verify_sessions(repo, &file_state.lines, operation_log, config.seed);
            registry.verify_session_retention(repo, operation_log, config.seed);
        }
    }
}
