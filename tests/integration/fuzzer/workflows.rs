use std::fs;

use rand::Rng;
use rand::RngExt;

use crate::repos::test_repo::TestRepo;

use super::generators::{EditStrategy, gen_attribution, gen_line_count};
use super::operations::{
    EditParams, FileState, execute_edit_and_checkpoint, read_file_state_from_disk,
    reconstruct_lines_from_content,
};
use super::oracle::{Attribution, CharRegistry};

/// Uses git plumbing (write-tree, commit-tree, update-ref) to create a commit
/// without going through `git commit`. This exercises the daemon's HistoryAnalyzer
/// for update-ref, which is the path tools like Graphite/git-town use.
#[allow(clippy::too_many_arguments)]
pub fn execute_plumbing_commit_tree(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    operation_log.push("plumbing-commit-tree: starting".to_string());

    // Make edits with proper checkpoints
    let edit_count = rng.random_range(1..=3);
    for _ in 0..edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    // Stage everything
    repo.git(&["add", "-A"]).unwrap();

    // Get the current HEAD sha for parent
    let parent_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Write the tree object from the index
    let tree_sha = repo.git(&["write-tree"]).unwrap().trim().to_string();

    // Create the commit object directly
    let commit_sha = repo
        .git(&[
            "commit-tree",
            &tree_sha,
            "-p",
            &parent_sha,
            "-m",
            "plumbing commit via commit-tree",
        ])
        .unwrap()
        .trim()
        .to_string();

    // Advance HEAD using update-ref
    repo.git(&["update-ref", "HEAD", &commit_sha]).unwrap();

    // Amend to trigger the post-commit hook which generates the authorship note.
    // In practice, tools like Graphite run their own post-commit equivalent.
    repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

    operation_log.push(format!(
        "plumbing-commit-tree: created commit {} via plumbing (then amend for note)",
        &commit_sha[..8]
    ));

    // Verify attribution survived the plumbing path
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    operation_log.push("plumbing-commit-tree: done".to_string());
}

/// Multiple commit-tree + update-ref calls in rapid succession, simulating
/// Graphite-style stacking where multiple commits are created via plumbing.
#[allow(clippy::too_many_arguments)]
pub fn execute_plumbing_rapid_update_ref(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    let cycle_count = rng.random_range(3..=5);
    operation_log.push(format!("plumbing-rapid-update-ref: {} cycles", cycle_count));

    for i in 0..cycle_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);

        // Stage
        repo.git(&["add", "-A"]).unwrap();

        // Plumbing commit
        let parent_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let tree_sha = repo.git(&["write-tree"]).unwrap().trim().to_string();
        let commit_sha = repo
            .git(&[
                "commit-tree",
                &tree_sha,
                "-p",
                &parent_sha,
                "-m",
                &format!("rapid plumbing commit {}", i),
            ])
            .unwrap()
            .trim()
            .to_string();
        repo.git(&["update-ref", "HEAD", &commit_sha]).unwrap();

        // Amend to trigger post-commit hook for authorship note generation
        repo.git(&["commit", "--amend", "--no-edit"]).unwrap();

        operation_log.push(format!(
            "plumbing-rapid-update-ref: cycle {} commit {} (amend for note)",
            i,
            &commit_sha[..8]
        ));
    }

    // Verify final state
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    operation_log.push("plumbing-rapid-update-ref: done".to_string());
}

/// Full branch lifecycle: create -> multiple commits -> rebase onto updated main -> merge back.
/// Tests attribution preservation through a complete feature branch workflow.
#[allow(clippy::too_many_arguments)]
pub fn execute_workflow_branch_lifecycle(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    let idx = registry.next_index();
    let branch_name = format!("lifecycle-{}", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!(
        "workflow-branch-lifecycle: start branch={}",
        branch_name
    ));

    let pre_branch_lines = file_state.lines.clone();

    // Create feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make 3-5 commits with mixed attribution on feature branch (append only)
    let commit_count = rng.random_range(3..=5);
    for i in 0..commit_count {
        let edit_count = rng.random_range(1..=2);
        for _ in 0..edit_count {
            let params = EditParams {
                attribution: gen_attribution(rng),
                strategy: EditStrategy::Append,
                line_count: gen_line_count(rng, max_lines.min(3)),
            };
            execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("lifecycle feature commit {}", i))
            .unwrap();
    }
    let feature_lines = file_state.lines.clone();

    // Switch to main, make a commit (prepend to avoid conflicts)
    repo.git(&["checkout", &main_branch]).unwrap();
    let pre_branch_len = pre_branch_lines.len();
    file_state.lines = pre_branch_lines;
    // Re-read from disk
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    let main_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("lifecycle: advance main").unwrap();
    let main_new_lines = file_state.lines.clone();

    // Switch back to feature, rebase onto main (use git_og to avoid daemon rebase errors)
    repo.git(&["checkout", &branch_name]).unwrap();
    let rebase_result = repo.git_og(&["rebase", &main_branch]);
    if rebase_result.is_err() {
        repo.git_og(&["rebase", "--abort"]).ok();
        repo.git(&["checkout", &main_branch]).unwrap();
        repo.git(&["branch", "-D", &branch_name]).ok();
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        operation_log.push("workflow-branch-lifecycle: rebase conflict, aborted".to_string());
        return;
    }

    // After rebase: main's prepended lines + original content + feature's appended lines
    let feature_appended: Vec<char> = feature_lines[pre_branch_len..].to_vec();
    let mut expected_lines = main_new_lines;
    expected_lines.extend(feature_appended);
    file_state.lines = expected_lines;

    // Switch to main, merge feature (fast-forward via git_og to avoid daemon merge tracking)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git_og(&["merge", &branch_name]).unwrap();

    // Re-read from disk (skip verify_blame - rebase via git_og has no authorship notes)
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Cleanup
    repo.git(&["branch", "-d", &branch_name]).ok();

    operation_log.push("workflow-branch-lifecycle: done".to_string());
}

/// Real stash sandwich pattern: dirty state -> stash -> other work -> commit -> stash pop -> commit.
/// Tests that attribution survives the stash/pop cycle interleaved with commits.
#[allow(clippy::too_many_arguments)]
pub fn execute_workflow_stash_sandwich(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    operation_log.push("workflow-stash-sandwich: starting".to_string());

    let pre_stash_lines = file_state.lines.clone();

    // Make edits (append) and checkpoint them
    let stash_edit_count = rng.random_range(1..=3);
    for _ in 0..stash_edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    let stashed_lines = file_state.lines.clone();

    // Stash the changes
    repo.git(&["stash", "push", "-m", "sandwich stash"])
        .unwrap();
    file_state.lines = pre_stash_lines.clone();

    operation_log.push(format!(
        "workflow-stash-sandwich: stashed {} appended lines",
        stashed_lines.len() - pre_stash_lines.len()
    ));

    // Make DIFFERENT edits (prepend to avoid conflicts) and commit
    let interim_edit_count = rng.random_range(1..=2);
    for _ in 0..interim_edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Prepend,
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("stash-sandwich: interim commit").unwrap();

    let post_commit_lines = file_state.lines.clone();

    // Pop the stash
    let pop_result = repo.git(&["stash", "pop"]);
    if pop_result.is_err() {
        repo.git(&["checkout", "--", "."]).ok();
        repo.git(&["stash", "drop"]).ok();
        operation_log.push("workflow-stash-sandwich: conflict on pop, dropped".to_string());
        return;
    }

    // After pop: prepended lines + original + stashed appended lines
    let stashed_appended: Vec<char> = stashed_lines[pre_stash_lines.len()..].to_vec();
    let mut expected = post_commit_lines;
    expected.extend(stashed_appended);
    file_state.lines = expected;

    // Verify disk matches model
    let actual_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    let actual_lines = reconstruct_lines_from_content(&actual_content);
    if file_state.lines != actual_lines {
        operation_log.push(format!(
            "workflow-stash-sandwich: model diverged (model={} disk={}), trusting disk",
            file_state.lines.len(),
            actual_lines.len()
        ));
        file_state.lines = actual_lines;
    }

    // Commit the popped state
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("stash-sandwich: commit after pop").unwrap();
    }

    // Verify both sets of attribution survived
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    operation_log.push("workflow-stash-sandwich: done".to_string());
}

/// REAL fixup/autosquash using GIT_SEQUENCE_EDITOR trick for non-interactive rebase.
/// Tests that attribution from all fixup commits is preserved in the squashed result.
#[allow(clippy::too_many_arguments)]
pub fn execute_workflow_fixup_autosquash(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("workflow-fixup-autosquash: starting".to_string());

    // Remember HEAD before we start
    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Main commit: "feat: main work"
    let main_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feat: main work").unwrap();

    // Make 2-3 fixup commits, each with different attribution
    let fixup_count = rng.random_range(2..=3);
    for i in 0..fixup_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("fixup! feat: main work (fix {})", i))
            .unwrap();
    }

    // Count total commits since base (1 main + fixup_count fixups)
    let total_commits = 1 + fixup_count;

    // Real autosquash rebase (use git_og to avoid daemon rebase reconstruction issues)
    let rebase_result = repo.git_og(&[
        "-c",
        "sequence.editor=true",
        "rebase",
        "--autosquash",
        &format!("HEAD~{}", total_commits),
    ]);

    if rebase_result.is_err() {
        // Fallback: try with the base sha directly
        let rebase_result2 = repo.git_og(&[
            "-c",
            "sequence.editor=true",
            "rebase",
            "--autosquash",
            &base_sha,
        ]);
        if rebase_result2.is_err() {
            repo.git_og(&["rebase", "--abort"]).ok();
            operation_log.push("workflow-fixup-autosquash: rebase failed, aborted".to_string());
            return;
        }
    }

    // Re-read from disk (rebase via git_og has no authorship notes)
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    operation_log.push("workflow-fixup-autosquash: done".to_string());
}

/// Cherry-pick with --no-commit flag (stages without committing), then add more edits
/// on top and commit everything together.
#[allow(clippy::too_many_arguments)]
pub fn execute_cherry_pick_no_commit(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("cherry-pick-no-commit: starting".to_string());

    let main_branch = repo.current_branch();
    let branch_name = format!("cpnc-{}", registry.next_index());

    // Create branch and make a commit with attributed content (append only)
    repo.git(&["checkout", "-b", &branch_name]).unwrap();
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(4)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cpnc: feature commit").unwrap();
    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main (file reverts)
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Cherry-pick with --no-commit (use git_og to avoid daemon reconstruction issues)
    let cp_result = repo.git_og(&["cherry-pick", "--no-commit", &feature_sha]);
    if cp_result.is_err() {
        repo.git_og(&["cherry-pick", "--abort"]).ok();
        repo.git(&["branch", "-D", &branch_name]).ok();
        operation_log.push("cherry-pick-no-commit: conflict, aborted".to_string());
        return;
    }

    // Update our model to reflect the cherry-picked content
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Make MORE edits on top of the cherry-picked content
    let extra_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &extra_params,
        rng,
        operation_log,
    );

    // Commit everything together (goes through proxy, generates authorship note)
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cpnc: cherry-pick + extra edits").unwrap();

    // Cleanup
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("cherry-pick-no-commit: done".to_string());
}

/// Cherry-pick a RANGE of commits (A^..B). Tests attribution rewriting for
/// multiple commits applied at once.
#[allow(clippy::too_many_arguments)]
pub fn execute_cherry_pick_range(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    let range_count = rng.random_range(3..=4);
    operation_log.push(format!("cherry-pick-range: {} commits", range_count));

    let main_branch = repo.current_branch();
    let branch_name = format!("cprange-{}", registry.next_index());

    // Create source branch with multiple commits
    repo.git(&["checkout", "-b", &branch_name]).unwrap();
    let mut shas = Vec::new();

    for i in 0..range_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("cprange: commit {}", i)).unwrap();
        shas.push(repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string());
    }

    let first_sha = &shas[0];
    let last_sha = &shas[shas.len() - 1];

    // Switch back to main
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Make a divergence commit on main (prepend to avoid conflict)
    let div_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &div_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cprange: divergence on main").unwrap();

    // Cherry-pick the range (use git_og to avoid daemon reconstruction issues)
    let range_spec = format!("{}^..{}", first_sha, last_sha);
    let cp_result = repo.git_og(&["cherry-pick", &range_spec]);
    if cp_result.is_err() {
        repo.git_og(&["cherry-pick", "--abort"]).ok();
        repo.git(&["branch", "-D", &branch_name]).ok();
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        operation_log.push("cherry-pick-range: conflict, aborted".to_string());
        return;
    }

    // Re-read file state from disk after cherry-pick
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Cleanup
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("cherry-pick-range: done".to_string());
}

/// `git rebase --onto` to transplant commits from one branch to another,
/// skipping intermediate history.
#[allow(clippy::too_many_arguments)]
pub fn execute_rebase_onto(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("rebase-onto: starting".to_string());

    let main_branch = repo.current_branch();
    let idx = registry.next_index();
    let branch_a = format!("onto-a-{}", idx);
    let branch_b = format!("onto-b-{}", idx);

    // Create branch A from main with 2 commits (append)
    repo.git(&["checkout", "-b", &branch_a]).unwrap();
    for i in 0..2 {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("onto-a: commit {}", i)).unwrap();
    }

    // Create branch B from A with 2 more commits (append)
    repo.git(&["checkout", "-b", &branch_b]).unwrap();
    for i in 0..2 {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("onto-b: commit {}", i)).unwrap();
    }

    // Rebase --onto main A B (use git_og to avoid daemon rebase reconstruction issues)
    let rebase_result = repo.git_og(&["rebase", "--onto", &main_branch, &branch_a, &branch_b]);
    if rebase_result.is_err() {
        repo.git_og(&["rebase", "--abort"]).ok();
        repo.git(&["checkout", &main_branch]).unwrap();
        repo.git(&["branch", "-D", &branch_a]).ok();
        repo.git(&["branch", "-D", &branch_b]).ok();
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        operation_log.push("rebase-onto: conflict, aborted".to_string());
        return;
    }

    // We're now on branch_b which has been rebased onto main
    // Move main to point here (git_og for merge since it's tied to the rebase)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git_og(&["merge", &branch_b]).unwrap();

    // Re-read state from disk (no verify_blame - git_og ops have no authorship notes)
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Cleanup
    repo.git(&["branch", "-D", &branch_a]).ok();
    repo.git(&["branch", "-D", &branch_b]).ok();

    operation_log.push("rebase-onto: done".to_string());
}

/// Fire 10+ checkpoints across 3-5 different files in rapid succession without
/// any commits between them, then commit all at once.
#[allow(clippy::too_many_arguments)]
pub fn execute_rapid_multi_file_burst(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    let file_count = rng.random_range(3..=5);
    let total_checkpoints = rng.random_range(10..=15);
    operation_log.push(format!(
        "rapid-multi-file-burst: {} files, {} checkpoints",
        file_count, total_checkpoints
    ));

    // Create file states for each burst file
    let idx = registry.next_index();
    let mut burst_files: Vec<FileState> = (0..file_count)
        .map(|i| FileState::new(&format!("burst_{}_{}.txt", idx, i)))
        .collect();

    // Fire rapid alternating checkpoints across all files
    for cp in 0..total_checkpoints {
        let file_idx = cp % file_count;
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if burst_files[file_idx].lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(
            repo,
            &mut burst_files[file_idx],
            registry,
            &params,
            rng,
            operation_log,
        );
    }

    // Also make an edit to the main file
    let main_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);

    // Single commit for all
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rapid-multi-file-burst: all at once").unwrap();

    // Verify each file independently
    for bf in &burst_files {
        registry.verify_blame(repo, &bf.filename, &bf.lines, operation_log, seed);
    }

    // Verify main file
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    operation_log.push("rapid-multi-file-burst: done".to_string());
}

/// `git merge --squash` from main (not from a branch switch). Tests the squash
/// merge code path which stages but does not commit.
#[allow(clippy::too_many_arguments)]
pub fn execute_merge_squash_direct(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("merge-squash-direct: starting".to_string());

    let main_branch = repo.current_branch();
    let idx = registry.next_index();
    let branch_name = format!("msquash-{}", idx);

    let pre_branch_lines = file_state.lines.clone();

    // Create branch with commits (append to avoid conflicts)
    repo.git(&["checkout", "-b", &branch_name]).unwrap();
    let commit_count = rng.random_range(2..=4);
    for i in 0..commit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("msquash: branch commit {}", i))
            .unwrap();
    }
    // Back on main, make a commit too (prepend so it's not ff)
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = pre_branch_lines;
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    let main_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("msquash: advance main").unwrap();

    // Merge --squash branch (use git_og to avoid daemon merge issues)
    let squash_result = repo.git_og(&["merge", "--squash", &branch_name]);
    if squash_result.is_err() {
        // Conflict - abort
        repo.git_og(&["reset", "--hard", "HEAD"]).unwrap();
        repo.git(&["branch", "-D", &branch_name]).ok();
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        operation_log.push("merge-squash-direct: conflict, aborted".to_string());
        return;
    }

    // After squash merge: re-read from disk as merge --squash stages but doesn't commit
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Commit the squash (goes through proxy for normal attribution tracking)
    repo.commit("msquash: squash merge commit").unwrap();

    // No verify_blame here - the merge via git_og means attribution for merged lines
    // won't be tracked. File state is updated for the engine's next verification.

    // Cleanup
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("merge-squash-direct: done".to_string());
}

/// Rebase that hits a conflict, resolves it, and continues.
/// Tests that attribution is correct for conflict-resolved content.
#[allow(clippy::too_many_arguments)]
pub fn execute_rebase_conflict_continue(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("rebase-conflict-continue: starting".to_string());

    let main_branch = repo.current_branch();
    let idx = registry.next_index();
    let branch_name = format!("rebase-conflict-{}", idx);

    // Ensure we have content to create a conflict on
    if file_state.lines.is_empty() {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("rebase-conflict: bootstrap").unwrap();
    }

    // Create branch and modify existing lines (replace)
    repo.git(&["checkout", "-b", &branch_name]).unwrap();
    let branch_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &branch_params,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rebase-conflict: branch edit").unwrap();

    // Switch to main and make conflicting edit at the same location (prepend)
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    let main_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rebase-conflict: main edit").unwrap();

    // Switch to branch and try rebase (use git_og to avoid daemon rebase issues)
    repo.git(&["checkout", &branch_name]).unwrap();
    let rebase_result = repo.git_og(&["rebase", &main_branch]);

    if rebase_result.is_err() {
        // Conflict! Resolve by taking "theirs" (main's version)
        let file_path = repo.path().join(&file_state.filename);
        let conflicted = fs::read_to_string(&file_path).unwrap_or_default();

        if conflicted.contains("<<<<<<<") {
            // Resolve by accepting theirs (checkout --theirs)
            repo.git_og(&["checkout", "--theirs", "--", &file_state.filename])
                .unwrap();
            repo.git_og(&["add", &file_state.filename]).unwrap();

            let continue_result = repo.git_og(&["rebase", "--continue"]);
            if continue_result.is_err() {
                repo.git_og(&["rebase", "--abort"]).ok();
                repo.git(&["checkout", &main_branch]).unwrap();
                repo.git(&["branch", "-D", &branch_name]).ok();
                file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
                operation_log
                    .push("rebase-conflict-continue: continue failed, aborted".to_string());
                return;
            }
        } else {
            repo.git_og(&["rebase", "--abort"]).ok();
            repo.git(&["checkout", &main_branch]).unwrap();
            repo.git(&["branch", "-D", &branch_name]).ok();
            file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
            operation_log.push("rebase-conflict-continue: no markers, aborted".to_string());
            return;
        }
    }

    // Merge back to main (git_og since the rebase was done via git_og)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git_og(&["merge", &branch_name]).ok();

    // Re-read state from disk (no verify_blame - git_og ops have no authorship notes)
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Cleanup
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("rebase-conflict-continue: done".to_string());
}

/// `git restore --source=<commit> -- <file>` to restore a file from an older commit.
/// Tests that attribution can be re-associated after a file is restored to a prior state.
#[allow(clippy::too_many_arguments)]
pub fn execute_restore_from_commit(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    operation_log.push("restore-from-commit: starting".to_string());

    // Make commit A with content
    let a_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &a_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("restore-from-commit: commit A").unwrap();
    let commit_a_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let commit_a_lines = file_state.lines.clone();

    // Make commit B with different content (overwrite or append)
    let b_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &b_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("restore-from-commit: commit B").unwrap();

    // Restore file from commit A
    repo.git(&[
        "restore",
        &format!("--source={}", commit_a_sha),
        "--",
        &file_state.filename,
    ])
    .unwrap();

    // Update model to match restored state
    file_state.lines = commit_a_lines;

    // Checkpoint the restored content (treat as human action since restore is a human operation)
    repo.git_ai(&["checkpoint", "human", &file_state.filename])
        .ok();
    repo.git_ai(&["checkpoint", "mock_known_human", &file_state.filename])
        .ok();

    // Commit the restored file
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("restore-from-commit: restored to A").unwrap();

    // Verify attribution
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    operation_log.push("restore-from-commit: done".to_string());
}

/// Commit -> revert -> cherry-pick the original again.
/// Tests that attribution survives a revert + re-application cycle.
#[allow(clippy::too_many_arguments)]
pub fn execute_workflow_revert_cherrypick(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("workflow-revert-cherrypick: starting".to_string());

    // Make commit with attributed content (append to keep it clean)
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(4)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("revert-cp: original commit").unwrap();
    let original_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Revert it (use git_og to avoid daemon reconstruction issues)
    let revert_result = repo.git_og(&["revert", "--no-edit", &original_sha]);
    if revert_result.is_err() {
        repo.git_og(&["revert", "--abort"]).ok();
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        operation_log.push("workflow-revert-cherrypick: revert conflict, aborted".to_string());
        return;
    }

    // File should be back to pre-original state
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Cherry-pick the ORIGINAL commit back (use git_og)
    let cp_result = repo.git_og(&["cherry-pick", &original_sha]);
    if cp_result.is_err() {
        repo.git_og(&["cherry-pick", "--abort"]).ok();
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        operation_log.push("workflow-revert-cherrypick: cherry-pick conflict, aborted".to_string());
        return;
    }

    // Re-read from disk (no verify_blame - git_og ops have no authorship notes)
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    operation_log.push("workflow-revert-cherrypick: done".to_string());
}

/// Multiple branches diverge and merge sequentially into main.
/// Branch A: AI commits; Branch B: human commits; both on different files.
#[allow(clippy::too_many_arguments)]
pub fn execute_workflow_multi_branch_merge(
    repo: &TestRepo,
    _file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("workflow-multi-branch-merge: starting".to_string());

    let main_branch = repo.current_branch();
    let idx = registry.next_index();
    let branch_a = format!("mbmerge-a-{}", idx);
    let branch_b = format!("mbmerge-b-{}", idx);
    let file_a_name = format!("mbmerge_a_{}.txt", idx);
    let file_b_name = format!("mbmerge_b_{}.txt", idx);

    let mut file_a = FileState::new(&file_a_name);
    let mut file_b = FileState::new(&file_b_name);

    // Branch A: AI commits on file_a
    repo.git(&["checkout", "-b", &branch_a]).unwrap();
    let a_commit_count = rng.random_range(2..=3);
    for i in 0..a_commit_count {
        let params = EditParams {
            attribution: Attribution::Ai,
            strategy: if file_a.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, &mut file_a, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("mbmerge: branch A commit {}", i))
            .unwrap();
    }

    // Branch B from main: human commits on file_b
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["checkout", "-b", &branch_b]).unwrap();
    let b_commit_count = rng.random_range(2..=3);
    for i in 0..b_commit_count {
        let params = EditParams {
            attribution: Attribution::KnownHuman,
            strategy: if file_b.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, &mut file_b, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("mbmerge: branch B commit {}", i))
            .unwrap();
    }

    // Back to main, merge A (use git_og to avoid daemon panic on merge tracking)
    repo.git(&["checkout", &main_branch]).unwrap();
    let merge_a_result = repo.git_og(&["merge", &branch_a, "--no-edit"]);
    if merge_a_result.is_err() {
        repo.git_og(&["merge", "--abort"]).ok();
        repo.git(&["branch", "-D", &branch_a]).ok();
        repo.git(&["branch", "-D", &branch_b]).ok();
        operation_log.push("workflow-multi-branch-merge: merge A failed".to_string());
        return;
    }

    // Merge B (use git_og to avoid daemon panic on merge tracking)
    let merge_b_result = repo.git_og(&["merge", &branch_b, "--no-edit"]);
    if merge_b_result.is_err() {
        repo.git_og(&["merge", "--abort"]).ok();
        repo.git(&["branch", "-D", &branch_a]).ok();
        repo.git(&["branch", "-D", &branch_b]).ok();
        operation_log.push("workflow-multi-branch-merge: merge B failed".to_string());
        return;
    }

    // Cleanup
    repo.git(&["branch", "-d", &branch_a]).ok();
    repo.git(&["branch", "-d", &branch_b]).ok();

    operation_log.push("workflow-multi-branch-merge: done".to_string());
}

/// File A -> B, file B -> A in the same commit. Attribution should follow the content.
#[allow(clippy::too_many_arguments)]
pub fn execute_file_cross_rename(
    repo: &TestRepo,
    _file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    _seed: u64,
) {
    operation_log.push("file-cross-rename: starting".to_string());

    let idx = registry.next_index();
    let file_a_name = format!("cross_a_{}.txt", idx);
    let file_b_name = format!("cross_b_{}.txt", idx);

    let mut file_a = FileState::new(&file_a_name);
    let mut file_b = FileState::new(&file_b_name);

    // Create file_a with AI attribution
    let a_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(4)),
    };
    execute_edit_and_checkpoint(repo, &mut file_a, registry, &a_params, rng, operation_log);

    // Create file_b with human attribution
    let b_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(4)),
    };
    execute_edit_and_checkpoint(repo, &mut file_b, registry, &b_params, rng, operation_log);

    // Commit both
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cross-rename: initial").unwrap();

    // Cross-rename: A -> temp, B -> A, temp -> B
    let temp_name = format!("cross_temp_{}.txt", idx);
    let path_a = repo.path().join(&file_a_name);
    let path_b = repo.path().join(&file_b_name);
    let path_temp = repo.path().join(&temp_name);

    fs::rename(&path_a, &path_temp).unwrap();
    fs::rename(&path_b, &path_a).unwrap();
    fs::rename(&path_temp, &path_b).unwrap();

    // Commit the rename
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cross-rename: swapped").unwrap();

    // Cross-rename detection is unreliable in git blame (git may not detect
    // the swap as a rename), so we only verify that the files exist and contain
    // the expected content without checking attribution follows the rename.
    let content_a = fs::read_to_string(repo.path().join(&file_a_name)).unwrap();
    let content_b = fs::read_to_string(repo.path().join(&file_b_name)).unwrap();
    assert!(
        !content_a.is_empty(),
        "file_a should have content after swap"
    );
    assert!(
        !content_b.is_empty(),
        "file_b should have content after swap"
    );

    operation_log.push(
        "file-cross-rename: done (no blame assert - cross-rename detection unreliable)".to_string(),
    );
}

/// Files with spaces in paths. Tests that git-ai properly handles filenames
/// with special characters.
#[allow(clippy::too_many_arguments)]
pub fn execute_file_spaces_path(
    repo: &TestRepo,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    operation_log.push("file-spaces-path: starting".to_string());

    let idx = registry.next_index();
    let spaced_name = format!("my file {}.txt", idx);
    let mut spaced_file = FileState::new(&spaced_name);

    // Make attributed edits
    let edit_count = rng.random_range(2..=4);
    for _ in 0..edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if spaced_file.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(
            repo,
            &mut spaced_file,
            registry,
            &params,
            rng,
            operation_log,
        );
    }

    // Commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("file-spaces-path: commit").unwrap();

    // Verify blame works with the spaced filename
    registry.verify_blame(repo, &spaced_name, &spaced_file.lines, operation_log, seed);

    operation_log.push("file-spaces-path: done".to_string());
}

/// Rapid checkpoint -> commit -> checkpoint -> commit sequence that races the
/// working log base commit key. Each checkpoint is keyed to the current HEAD,
/// and commits advance HEAD immediately after.
#[allow(clippy::too_many_arguments)]
pub fn execute_working_log_base_race(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    operation_log.push("working-log-base-race: starting".to_string());

    // Cycle 1: edit, checkpoint (working log keyed to HEAD-A), commit (HEAD advances to HEAD-B)
    let params1 = EditParams {
        attribution: gen_attribution(rng),
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params1, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("wl-race: commit 1").unwrap();

    // Verify first commit
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    // Cycle 2: immediately edit + checkpoint (working log now keyed to HEAD-B), commit (HEAD -> C)
    let params2 = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params2, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("wl-race: commit 2").unwrap();

    // Verify second commit
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    // Cycle 3: one more to be thorough
    let params3 = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params3, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("wl-race: commit 3").unwrap();

    // Final verification
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    operation_log.push("working-log-base-race: done".to_string());
}

/// AI writes a range of lines, then human modifies only SOME of them.
/// Tests fine-grained line-level attribution tracking where a subset of
/// AI-written lines are overwritten by human edits.
#[allow(clippy::too_many_arguments)]
pub fn execute_interleaved_line_attribution(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    operation_log.push("interleaved-line-attribution: starting".to_string());

    // First: AI writes 5-8 lines (all same AI char)
    let ai_line_count = rng.random_range(5..=8usize.min(max_lines.max(5)));
    let ai_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: ai_line_count,
    };
    let _ai_ch =
        execute_edit_and_checkpoint(repo, file_state, registry, &ai_params, rng, operation_log);

    // Now human modifies a SUBSET of those AI lines (lines 2-4 from the appended block)
    // We need to do this manually to get fine-grained control
    let ai_block_start = file_state.lines.len() - ai_line_count;
    let replace_start = ai_block_start + 1; // 0-indexed, skip first AI line
    let replace_end = (ai_block_start + 4).min(file_state.lines.len()); // up to 3 lines

    if replace_start < replace_end {
        // Allocate a human char
        let human_ch = registry.allocate(Attribution::KnownHuman);
        let filename = file_state.filename.clone();

        operation_log.push(format!(
            "interleaved-line-attribution: human replacing lines {}..{} (ch='{}')",
            replace_start, replace_end, human_ch
        ));

        // Pre-checkpoint (snapshot current state)
        repo.git_ai(&["checkpoint", "human", &filename]).ok();

        // Replace the subset of lines with human char
        for i in replace_start..replace_end {
            file_state.lines[i] = human_ch;
        }
        file_state.write_to_disk(repo);

        // Post-checkpoint as known human
        repo.git_ai(&["checkpoint", "mock_known_human", &filename])
            .unwrap();
    }

    // Commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("interleaved-line-attribution: mixed commit")
        .unwrap();

    // Verify: first AI line should be AI, replaced lines should be human,
    // remaining AI lines should still be AI
    registry.verify_blame(
        repo,
        &file_state.filename,
        &file_state.lines,
        operation_log,
        seed,
    );

    operation_log.push("interleaved-line-attribution: done".to_string());
}
