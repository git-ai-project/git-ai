use std::fs;

use rand::Rng;
use rand::RngExt;

use crate::repos::test_repo::TestRepo;

use super::generators::{EditStrategy, gen_attribution, gen_line_count};
use super::oracle::{Attribution, CharRegistry};

/// Tracks the current state of a file as a list of characters (one per line).
#[derive(Debug, Clone)]
pub struct FileState {
    pub lines: Vec<char>,
    pub filename: String,
}

impl FileState {
    pub fn new(filename: &str) -> Self {
        Self {
            lines: Vec::new(),
            filename: filename.to_string(),
        }
    }

    /// Apply an edit strategy, inserting `line_count` lines of character `ch`.
    pub fn apply_edit(
        &mut self,
        strategy: EditStrategy,
        ch: char,
        line_count: usize,
        rng: &mut impl Rng,
    ) {
        match strategy {
            EditStrategy::Append => {
                for _ in 0..line_count {
                    self.lines.push(ch);
                }
            }
            EditStrategy::Prepend => {
                let new_lines: Vec<char> = vec![ch; line_count];
                self.lines.splice(0..0, new_lines);
            }
            EditStrategy::InsertRandom => {
                let pos = if self.lines.is_empty() {
                    0
                } else {
                    rng.random_range(0..=self.lines.len())
                };
                let new_lines: Vec<char> = vec![ch; line_count];
                self.lines.splice(pos..pos, new_lines);
            }
            EditStrategy::ReplaceRandom => {
                if self.lines.is_empty() {
                    for _ in 0..line_count {
                        self.lines.push(ch);
                    }
                } else {
                    let max_start = self.lines.len().saturating_sub(1);
                    let start = rng.random_range(0..=max_start);
                    let end = (start + line_count).min(self.lines.len());
                    let replacement: Vec<char> = vec![ch; end - start];
                    self.lines.splice(start..end, replacement);
                }
            }
            EditStrategy::DeleteAndInsert => {
                if self.lines.is_empty() {
                    for _ in 0..line_count {
                        self.lines.push(ch);
                    }
                } else {
                    let max_start = self.lines.len().saturating_sub(1);
                    let start = rng.random_range(0..=max_start);
                    let delete_count = rng
                        .random_range(1..=(self.lines.len() - start).max(1))
                        .min(5);
                    let end = (start + delete_count).min(self.lines.len());
                    let new_lines: Vec<char> = vec![ch; line_count];
                    self.lines.splice(start..end, new_lines);
                }
            }
            EditStrategy::OverwriteAll => {
                self.lines.clear();
                for _ in 0..line_count {
                    self.lines.push(ch);
                }
            }
        }
    }

    /// Write the file to disk. Each line is the char repeated deterministically.
    pub fn write_to_disk(&self, repo: &TestRepo) {
        let path = repo.path().join(&self.filename);
        let mut content = String::new();
        for &ch in &self.lines {
            let repeat_count = (ch as usize % 16) + 5;
            for _ in 0..repeat_count {
                content.push(ch);
            }
            content.push('\n');
        }
        fs::write(&path, &content).unwrap_or_else(|e| {
            panic!("Failed to write file '{}': {}", self.filename, e);
        });
    }
}

pub struct EditParams {
    pub attribution: Attribution,
    pub strategy: EditStrategy,
    pub line_count: usize,
}

/// Execute an edit with proper checkpoint calls and return the allocated character.
pub fn execute_edit_and_checkpoint(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    params: &EditParams,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) -> char {
    let EditParams {
        attribution,
        strategy,
        line_count,
    } = params;
    let ch = registry.allocate(*attribution);
    let filename = file_state.filename.clone();

    operation_log.push(format!(
        "edit: ch='{}' attr={} strategy={:?} lines={} file={}",
        ch, attribution, strategy, line_count, filename
    ));

    match attribution {
        Attribution::Ai => {
            // AI: pre-edit "human" to snapshot current state, then write, then "mock_ai"
            repo.git_ai(&["checkpoint", "human", &filename]).ok();
            file_state.apply_edit(*strategy, ch, *line_count, rng);
            file_state.write_to_disk(repo);
            repo.git_ai(&["checkpoint", "mock_ai", &filename]).unwrap();
        }
        Attribution::KnownHuman => {
            // Known human: pre-edit "human" to snapshot, then write, then "mock_known_human"
            repo.git_ai(&["checkpoint", "human", &filename]).ok();
            file_state.apply_edit(*strategy, ch, *line_count, rng);
            file_state.write_to_disk(repo);
            repo.git_ai(&["checkpoint", "mock_known_human", &filename])
                .unwrap();
        }
    }

    ch
}

/// Stage all and commit.
pub fn execute_commit(repo: &TestRepo, message: &str, operation_log: &mut Vec<String>) {
    operation_log.push(format!("commit: {}", message));
    repo.git(&["add", "-A"]).unwrap();
    repo.commit(message).unwrap();
}

/// Amend chain: edit the file N times, amending the same commit each time.
/// This is pathological because it repeatedly rewrites history with different
/// attribution types overlapping on the same commit.
#[allow(clippy::too_many_arguments)]
pub fn execute_amend_chain(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    chain_length: usize,
    max_lines: usize,
    allow_destructive: bool,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push(format!("amend-chain: starting (length={})", chain_length));

    for i in 0..chain_length {
        let strategy = if allow_destructive && file_state.lines.len() > 2 {
            EditStrategy::random(rng)
        } else if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        };

        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy,
            line_count: gen_line_count(rng, max_lines),
        };

        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);

        repo.git(&["add", "-A"]).unwrap();
        repo.git(&[
            "commit",
            "--amend",
            "-m",
            &format!("amend chain step {}", i),
        ])
        .unwrap();

        operation_log.push(format!("amend-chain: step {} complete", i));
    }

    operation_log.push("amend-chain: done".to_string());
}

/// Fast-forward merge: create a branch with interleaved edits on a separate file,
/// then merge back. Tests attribution preservation through branch merge.
/// NOTE: cherry-pick is intentionally excluded because the daemon has a known
/// reflog ambiguity issue (ambiguous HEAD reflog chain) in repos with many commits.
#[allow(clippy::too_many_arguments)]
pub fn execute_ff_merge(
    repo: &TestRepo,
    registry: &mut CharRegistry,
    max_edits: usize,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    let idx = registry.next_index();
    let branch_name = format!("ffmerge-{}", idx);
    let merge_filename = format!("merge_{}.txt", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!(
        "ff-merge: start branch={} file={}",
        branch_name, merge_filename
    ));

    let mut merge_file_state = FileState::new(&merge_filename);

    // Create branch from current HEAD
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // On the branch: multiple interleaved edits with different attribution types
    let edit_count = rng.random_range(2..=max_edits.min(4));
    for _ in 0..edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if merge_file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random(rng)
            },
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(
            repo,
            &mut merge_file_state,
            registry,
            &params,
            rng,
            operation_log,
        );
    }

    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feature branch commit").unwrap();

    // Switch back to main and fast-forward merge
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", &branch_name]).unwrap();

    // Verify the merged file's attribution
    registry.verify_blame(
        repo,
        &merge_filename,
        &merge_file_state.lines,
        operation_log,
        seed,
    );

    // Cleanup
    repo.git(&["branch", "-d", &branch_name]).unwrap();

    operation_log.push(format!("ff-merge: done file={}", merge_filename));
}

/// Rebase that operates on the SAME main file.
/// Creates divergence by appending to the file on the feature branch
/// and prepending on main, then rebases without conflicts.
#[allow(clippy::too_many_arguments)]
pub fn execute_rebase_same_file(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_edits: usize,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let branch_name = format!("rebase-{}", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!("rebase-same-file: start branch={}", branch_name));

    // Snapshot current file state
    let pre_rebase_len = file_state.lines.len();

    // Create feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // On feature: APPEND lines (to avoid conflicts with main's prepend)
    let feature_edit_count = rng.random_range(1..=max_edits.min(3));
    for _ in 0..feature_edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rebase feature commit").unwrap();
    let feature_lines = file_state.lines.clone();

    // Switch to main, PREPEND lines (to avoid conflicts with feature's append)
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = file_state.lines[..pre_rebase_len].to_vec();

    // Re-read from disk to be safe
    let main_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    file_state.lines = reconstruct_lines_from_content(&main_content);

    let main_edit_count = rng.random_range(1..=max_edits.min(2));
    for _ in 0..main_edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Prepend,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("advance main for rebase").unwrap();
    let main_new_lines = file_state.lines.clone();

    // Rebase feature onto main
    repo.git(&["checkout", &branch_name]).unwrap();
    repo.git(&["rebase", &main_branch]).unwrap();

    // After rebase: main's prepended lines + original content + feature's appended lines
    let feature_appended: Vec<char> = feature_lines[pre_rebase_len..].to_vec();
    let mut expected_lines = main_new_lines;
    expected_lines.extend(feature_appended);
    file_state.lines = expected_lines;

    // Merge back to main (fast-forward)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", &branch_name]).unwrap();

    // Cleanup
    repo.git(&["branch", "-d", &branch_name]).unwrap();

    // Verify file on disk matches our model
    let actual_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    let actual_lines = reconstruct_lines_from_content(&actual_content);
    assert_eq!(
        file_state.lines, actual_lines,
        "File state model diverged from disk after rebase!\nModel: {:?}\nDisk: {:?}",
        file_state.lines, actual_lines
    );

    operation_log.push("rebase-same-file: done".to_string());
}

/// Squash merge operating on the SAME main file.
/// Creates a branch with multiple commits appending to the file, then squash-merges back.
/// Each commit on the branch has multiple interleaved edits.
#[allow(clippy::too_many_arguments)]
pub fn execute_squash_same_file(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let branch_name = format!("squash-{}", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!("squash-same-file: start branch={}", branch_name));

    // Snapshot pre-squash state
    let pre_squash_lines = file_state.lines.clone();

    // Create feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make 2-4 commits on the branch, each with multiple interleaved edits
    let commit_count = rng.random_range(2..=4u32);
    for i in 0..commit_count {
        let edit_count = rng.random_range(1..=4);
        for _ in 0..edit_count {
            let params = EditParams {
                attribution: gen_attribution(rng),
                strategy: EditStrategy::Append,
                line_count: gen_line_count(rng, max_lines),
            };
            execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("squash branch commit {}", i + 1))
            .unwrap();
    }

    let final_lines = file_state.lines.clone();

    // Switch back to main (file state reverts to pre-squash)
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = pre_squash_lines;

    // Squash merge
    repo.git(&["merge", "--squash", &branch_name]).unwrap();
    file_state.lines = final_lines;

    repo.git(&["commit", "-m", "squash merged"]).unwrap();

    // Cleanup
    repo.git(&["branch", "-D", &branch_name]).unwrap();

    operation_log.push(format!(
        "squash-same-file: done ({} commits squashed)",
        commit_count
    ));
}

/// Fire rapid checkpoints on a secondary file (stresses daemon with cross-file
/// interleaving before the main commit).
pub fn execute_interleaved_multi_file(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let burst_count = rng.random_range(3..=7);
    operation_log.push(format!(
        "multi-file-burst: {} rapid edits on {}",
        burst_count, file_state.filename
    ));

    for _ in 0..burst_count {
        let strategy = if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random(rng)
        };
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
}

/// Partial staging: edit the file with multiple attribution types, but only stage
/// a subset of lines (using line-range staging via a patch file). The unstaged lines
/// remain in the working directory for the next commit. This is extremely pathological
/// because it forces git-ai to correctly split working log entries between committed
/// and uncommitted attribution.
#[allow(clippy::too_many_arguments)]
pub fn execute_partial_stage_commit(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) -> PartialStageResult {
    operation_log.push("partial-stage: starting".to_string());

    let pre_edit_lines = file_state.lines.clone();

    // Make 2-4 edits with different attributions, always appending to avoid conflicts
    let edit_count = rng.random_range(2..=4);
    let mut edits_made: Vec<(char, Attribution, usize)> = Vec::new();

    for _ in 0..edit_count {
        let attr = gen_attribution(rng);
        let line_count = gen_line_count(rng, max_lines.min(4));
        let params = EditParams {
            attribution: attr,
            strategy: EditStrategy::Append,
            line_count,
        };
        let ch =
            execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        edits_made.push((ch, attr, line_count));
    }

    // Decide how many of the NEW lines to stage (always stage at least 1 line, leave at least 1)
    let new_line_count = file_state.lines.len() - pre_edit_lines.len();
    if new_line_count < 2 {
        // Not enough lines to meaningfully partial-stage; just commit everything
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("partial-stage: degenerate full commit")
            .unwrap();
        operation_log
            .push("partial-stage: degenerate (too few new lines), full commit".to_string());
        return PartialStageResult {
            committed_lines: file_state.lines.clone(),
            unstaged_lines: vec![],
        };
    }

    let lines_to_stage = rng.random_range(1..new_line_count);

    // Write only the staged portion to a temp version, use `git add -p` simulation
    // Strategy: write the full file, then use `git add` with a crafted patch
    // Simpler approach: write staged version, add it, then write full version back
    let staged_lines: Vec<char> = pre_edit_lines
        .iter()
        .chain(file_state.lines[pre_edit_lines.len()..pre_edit_lines.len() + lines_to_stage].iter())
        .copied()
        .collect();

    let full_lines = file_state.lines.clone();

    // Write the "staged" version and add it
    let staged_state = FileState {
        lines: staged_lines.clone(),
        filename: file_state.filename.clone(),
    };
    staged_state.write_to_disk(repo);
    repo.git(&["add", &file_state.filename]).unwrap();

    // Write back the full version (unstaged changes remain in working tree)
    file_state.write_to_disk(repo);

    operation_log.push(format!(
        "partial-stage: staging {}/{} new lines",
        lines_to_stage, new_line_count
    ));

    // Commit only staged changes
    repo.commit("partial stage commit").unwrap();

    // After commit: the committed state is staged_lines, working tree has full_lines
    // But git-ai's working log should carry over the unstaged attribution
    let committed_lines = staged_lines;
    let unstaged_portion: Vec<char> = full_lines[pre_edit_lines.len() + lines_to_stage..].to_vec();

    // Update file_state to reflect what's on disk (full version)
    file_state.lines = full_lines;

    operation_log.push(format!(
        "partial-stage: committed {} lines, {} lines remain unstaged",
        committed_lines.len(),
        unstaged_portion.len()
    ));

    PartialStageResult {
        committed_lines,
        unstaged_lines: unstaged_portion,
    }
}

pub struct PartialStageResult {
    pub committed_lines: Vec<char>,
    pub unstaged_lines: Vec<char>,
}

/// Multi-file partial staging: edit multiple files but only commit some of them.
/// This tests that attribution for uncommitted files is preserved across commits.
#[allow(clippy::too_many_arguments)]
pub fn execute_selective_file_commit(
    repo: &TestRepo,
    file_states: &mut [&mut FileState],
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    if file_states.len() < 2 {
        return;
    }

    operation_log.push(format!(
        "selective-file-commit: editing {} files",
        file_states.len()
    ));

    // Edit ALL files
    for file_state in file_states.iter_mut() {
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
    }

    // Only stage and commit the FIRST file, leave others dirty
    let committed_filename = file_states[0].filename.clone();
    repo.git(&["add", &committed_filename]).unwrap();
    repo.commit("selective file commit").unwrap();

    operation_log.push(format!(
        "selective-file-commit: committed only '{}', others remain dirty",
        committed_filename
    ));
}

/// Hard reset to a previous commit, discarding all working changes.
/// This is pathological because it forces git-ai to handle the case where
/// checkpointed attribution is completely thrown away.
pub fn execute_hard_reset(
    repo: &TestRepo,
    file_state: &mut FileState,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("hard-reset: resetting to HEAD~1".to_string());

    // Reset to parent commit
    let result = repo.git(&["reset", "--hard", "HEAD~1"]);
    if result.is_err() {
        operation_log.push("hard-reset: failed (probably at root commit), skipping".to_string());
        return;
    }

    // Re-read file state from disk
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        let content = fs::read_to_string(&path).unwrap();
        file_state.lines = reconstruct_lines_from_content(&content);
    } else {
        file_state.lines.clear();
    }

    operation_log.push(format!(
        "hard-reset: done, file now has {} lines",
        file_state.lines.len()
    ));
}

/// Soft reset + re-commit: resets HEAD but keeps changes staged, then re-commits.
/// Tests that attribution survives through soft reset cycles.
#[allow(clippy::too_many_arguments)]
pub fn execute_soft_reset_recommit(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("soft-reset-recommit: starting".to_string());

    // Soft reset to parent (keeps changes in index)
    let result = repo.git(&["reset", "--soft", "HEAD~1"]);
    if result.is_err() {
        operation_log.push("soft-reset-recommit: failed (root commit), skipping".to_string());
        return;
    }

    // Make additional edits on top of the soft-reset state
    let extra_edits = rng.random_range(1..=3);
    for _ in 0..extra_edits {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    // Stage everything and recommit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("soft-reset recommit with extra changes")
        .unwrap();

    operation_log.push("soft-reset-recommit: done".to_string());
}

/// Checkout file from HEAD, discarding working tree changes for a specific file.
/// This simulates `git checkout -- <file>` which throws away uncommitted edits.
/// Pathological because checkpoints were fired for the discarded content.
#[allow(clippy::too_many_arguments)]
pub fn execute_checkout_discard(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push(format!(
        "checkout-discard: discarding changes to '{}'",
        file_state.filename
    ));

    // Make edits and checkpoint them (this data should be thrown away)
    let doomed_edits = rng.random_range(1..=4);
    for _ in 0..doomed_edits {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random(rng)
            },
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    operation_log.push(format!(
        "checkout-discard: made {} doomed edits, now discarding",
        doomed_edits
    ));

    // Discard all working tree changes for this file
    repo.git(&["checkout", "--", &file_state.filename]).unwrap();

    // Re-read file state from disk (reverts to last committed version)
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        let content = fs::read_to_string(&path).unwrap();
        file_state.lines = reconstruct_lines_from_content(&content);
    } else {
        file_state.lines.clear();
    }

    operation_log.push(format!(
        "checkout-discard: file reverted to {} lines",
        file_state.lines.len()
    ));
}

/// Stash + pop cycle: make edits, stash them, make more edits and commit,
/// then pop the stash. Tests that stashed attribution is preserved and merged.
#[allow(clippy::too_many_arguments)]
pub fn execute_stash_pop_cycle(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("stash-pop: starting".to_string());

    // Make edits that will be stashed
    let stash_edits = rng.random_range(1..=3);
    let pre_stash_lines = file_state.lines.clone();
    for _ in 0..stash_edits {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    let stashed_lines = file_state.lines.clone();

    // Stash the changes
    repo.git(&["stash", "push", "-m", "fuzzer stash"]).unwrap();
    file_state.lines = pre_stash_lines.clone();

    operation_log.push(format!(
        "stash-pop: stashed {} new lines",
        stashed_lines.len() - pre_stash_lines.len()
    ));

    // Make DIFFERENT edits on the clean state and commit them
    // Use prepend to avoid conflict with the stashed appended lines
    let interim_edits = rng.random_range(1..=2);
    for _ in 0..interim_edits {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Prepend,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("interim commit before stash pop").unwrap();

    let post_commit_lines = file_state.lines.clone();

    // Pop the stash - this should merge the stashed appended lines
    let pop_result = repo.git(&["stash", "pop"]);
    if pop_result.is_err() {
        // Stash pop can fail with conflicts; in that case, abort and skip
        repo.git(&["checkout", "--", "."]).ok();
        repo.git(&["stash", "drop"]).ok();
        operation_log.push("stash-pop: conflict on pop, dropped stash".to_string());
        // File state remains as post_commit_lines
        return;
    }

    // After successful pop: prepended lines + original lines + stashed appended lines
    let stashed_appended: Vec<char> = stashed_lines[pre_stash_lines.len()..].to_vec();
    let mut expected = post_commit_lines;
    expected.extend(stashed_appended);
    file_state.lines = expected;

    // Verify disk matches model
    let actual_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    let actual_lines = reconstruct_lines_from_content(&actual_content);
    if file_state.lines != actual_lines {
        // Model diverged - trust disk
        operation_log.push(format!(
            "stash-pop: model diverged from disk (model={} disk={}), trusting disk",
            file_state.lines.len(),
            actual_lines.len()
        ));
        file_state.lines = actual_lines;
    }

    operation_log.push("stash-pop: done".to_string());
}

/// Branch switch with dirty working tree: create a new branch, make edits,
/// switch back WITHOUT committing (git allows this if no conflicts), then commit
/// on the original branch. Tests that attribution follows the commit, not the branch.
#[allow(clippy::too_many_arguments)]
pub fn execute_branch_switch_dirty(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let temp_branch = format!("dirty-switch-{}", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!(
        "branch-switch-dirty: start temp_branch={}",
        temp_branch
    ));

    // Create and switch to temp branch
    repo.git(&["checkout", "-b", &temp_branch]).unwrap();

    // Make edits and checkpoint on the temp branch
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

    // Switch back to main WITHOUT committing (dirty switch)
    // This should work since we're on the same commit and changes are compatible
    let switch_result = repo.git(&["checkout", &main_branch]);
    if switch_result.is_err() {
        // Checkout fails if there are conflicts; commit on temp branch instead
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("forced commit on temp branch").unwrap();
        repo.git(&["checkout", &main_branch]).unwrap();
        repo.git(&["merge", &temp_branch]).unwrap();
        repo.git(&["branch", "-d", &temp_branch]).unwrap();
        operation_log.push("branch-switch-dirty: had to commit on temp (conflicts)".to_string());
        return;
    }

    // Now commit these changes on main (attribution was checkpointed on temp branch)
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("committed dirty changes after branch switch")
        .unwrap();

    // Cleanup temp branch
    repo.git(&["branch", "-d", &temp_branch]).unwrap();

    operation_log.push("branch-switch-dirty: done (committed on main after switch)".to_string());
}

/// Interleaved partial staging across multiple files: edit files A and B,
/// stage only A, commit, then stage B and commit. Verifies attribution is correct
/// for both commits independently.
#[allow(clippy::too_many_arguments)]
pub fn execute_interleaved_partial_commits(
    repo: &TestRepo,
    file_state_a: &mut FileState,
    file_state_b: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("interleaved-partial: starting".to_string());

    // Edit both files with checkpoints
    let edits_a = rng.random_range(1..=3);
    for _ in 0..edits_a {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state_a.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state_a, registry, &params, rng, operation_log);
    }

    let edits_b = rng.random_range(1..=3);
    for _ in 0..edits_b {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state_b.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state_b, registry, &params, rng, operation_log);
    }

    // Stage ONLY file A and commit
    repo.git(&["add", &file_state_a.filename]).unwrap();
    repo.commit("partial: only file A").unwrap();

    operation_log.push(format!(
        "interleaved-partial: committed '{}', '{}' still dirty",
        file_state_a.filename, file_state_b.filename
    ));

    // Now stage file B and commit
    repo.git(&["add", &file_state_b.filename]).unwrap();
    repo.commit("partial: only file B").unwrap();

    operation_log.push("interleaved-partial: committed file B".to_string());
}

/// Hard reset then re-edit: reset to a previous state, then make new edits
/// with different attribution. Extremely pathological because the daemon
/// must correctly handle the HEAD change and not carry over stale attribution.
#[allow(clippy::too_many_arguments)]
pub fn execute_reset_and_reedit(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    // Need at least 2 commits to reset
    let log_output = repo.git(&["log", "--oneline"]).unwrap();
    let commit_count = log_output.lines().count();
    if commit_count < 3 {
        operation_log.push("reset-reedit: skipped (not enough commits)".to_string());
        return;
    }

    operation_log.push("reset-reedit: starting".to_string());

    // Reset to HEAD~1
    repo.git(&["reset", "--hard", "HEAD~1"]).unwrap();

    // Re-read file state from disk
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        let content = fs::read_to_string(&path).unwrap();
        file_state.lines = reconstruct_lines_from_content(&content);
    } else {
        file_state.lines.clear();
    }

    operation_log.push(format!(
        "reset-reedit: reset done, file has {} lines",
        file_state.lines.len()
    ));

    // Make new edits with fresh attribution
    let edit_count = rng.random_range(2..=4);
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

    // Commit the new state
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("post-reset new edits").unwrap();

    operation_log.push("reset-reedit: done".to_string());
}

/// Edit, checkpoint, then overwrite with different content before committing.
/// This creates a situation where the checkpointed state doesn't match what gets committed.
/// The final checkpoint before commit should win.
#[allow(clippy::too_many_arguments)]
pub fn execute_checkpoint_then_overwrite(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("checkpoint-overwrite: starting".to_string());

    // First: make an AI edit and checkpoint it
    let first_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    let first_ch = execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &first_params,
        rng,
        operation_log,
    );

    // Now OVERWRITE those lines with a human edit (different char, different attribution)
    // This simulates: AI writes code, human immediately rewrites it before commit
    let overwrite_lines = file_state.lines.iter().filter(|&&c| c == first_ch).count();
    if overwrite_lines > 0 {
        let second_params = EditParams {
            attribution: Attribution::KnownHuman,
            strategy: EditStrategy::OverwriteAll,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(
            repo,
            file_state,
            registry,
            &second_params,
            rng,
            operation_log,
        );
    }

    operation_log.push("checkpoint-overwrite: done (AI overwritten by human)".to_string());
}

/// File rename: rename a file with `git mv`, then continue editing.
/// Tests that attribution follows the file through rename.
#[allow(clippy::too_many_arguments)]
pub fn execute_file_rename(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let old_name = file_state.filename.clone();
    let new_name = format!("renamed_{}.txt", idx);

    operation_log.push(format!("file-rename: '{}' -> '{}'", old_name, new_name));

    // Commit current state first so rename is clean
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("pre-rename commit").unwrap();
    }

    // Rename via git mv
    repo.git(&["mv", &old_name, &new_name]).unwrap();
    repo.commit("rename file").unwrap();

    file_state.filename = new_name.clone();

    // Make edits after rename
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("post-rename edits").unwrap();

    operation_log.push(format!("file-rename: done, file is now '{}'", new_name));
}

/// Delete a file and recreate it with different content.
/// Extremely pathological: the old attribution should be gone and new attribution
/// should be tracked from scratch.
#[allow(clippy::too_many_arguments)]
pub fn execute_delete_and_recreate(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push(format!(
        "delete-recreate: deleting '{}'",
        file_state.filename
    ));

    // Commit any pending changes first
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("pre-delete commit").unwrap();
    }

    // Delete the file
    repo.git(&["rm", &file_state.filename]).unwrap();
    repo.commit("delete file").unwrap();
    file_state.lines.clear();

    // Recreate with fresh content
    let edit_count = rng.random_range(2..=5);
    for _ in 0..edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("recreate file with new content").unwrap();

    operation_log.push(format!(
        "delete-recreate: done, file has {} new lines",
        file_state.lines.len()
    ));
}

/// Move file into a subdirectory, then continue editing.
/// Tests path normalization and subdirectory attribution tracking.
#[allow(clippy::too_many_arguments)]
pub fn execute_move_to_subdir(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let subdir = format!("subdir_{}", idx);
    let old_name = file_state.filename.clone();
    let new_name = format!("{}/{}", subdir, old_name);

    operation_log.push(format!("move-to-subdir: '{}' -> '{}'", old_name, new_name));

    // Commit pending changes
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("pre-move commit").unwrap();
    }

    // Create subdir and move file
    fs::create_dir_all(repo.path().join(&subdir)).unwrap();
    repo.git(&["mv", &old_name, &new_name]).unwrap();
    repo.commit("move file to subdirectory").unwrap();

    file_state.filename = new_name.clone();

    // Edit after move
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("post-move edits").unwrap();

    operation_log.push(format!("move-to-subdir: done, file is now '{}'", new_name));
}

/// Mixed reset (--mixed): resets HEAD but keeps changes in working tree (unstaged).
/// Then makes new edits on top and commits. Tests that old attribution from the
/// previous commit's working log is properly reconstructed.
#[allow(clippy::too_many_arguments)]
pub fn execute_mixed_reset(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let log_output = repo.git(&["log", "--oneline"]).unwrap();
    let commit_count = log_output.lines().count();
    if commit_count < 3 {
        operation_log.push("mixed-reset: skipped (not enough commits)".to_string());
        return;
    }

    operation_log.push("mixed-reset: starting (HEAD~1)".to_string());

    // Mixed reset keeps changes in working tree but unstaged
    repo.git(&["reset", "HEAD~1"]).unwrap();

    // The file on disk is unchanged (mixed reset), but HEAD moved back
    // Make additional edits
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

    // Stage everything and commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("recommit after mixed reset with new edits")
        .unwrap();

    operation_log.push("mixed-reset: done".to_string());
}

/// Rapid-fire checkpoint burst: fire many checkpoints in quick succession
/// on the same file without any commits in between. This stress-tests the
/// daemon's sequencer ordering and deduplication logic.
#[allow(clippy::too_many_arguments)]
pub fn execute_rapid_checkpoint_burst(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let burst_size = rng.random_range(5..=15);
    operation_log.push(format!(
        "rapid-burst: {} rapid checkpoints on '{}'",
        burst_size, file_state.filename
    ));

    // Alternate rapidly between AI and human checkpoints
    for i in 0..burst_size {
        let attr = if i % 3 == 0 {
            Attribution::KnownHuman
        } else {
            Attribution::Ai
        };
        let params = EditParams {
            attribution: attr,
            strategy: if file_state.lines.is_empty() || i % 2 == 0 {
                EditStrategy::Append
            } else {
                EditStrategy::Prepend
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    // Single commit after all the rapid checkpoints
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("commit after rapid checkpoint burst").unwrap();

    operation_log.push(format!(
        "rapid-burst: done, file now has {} lines",
        file_state.lines.len()
    ));
}

/// Empty commit interleaved with real work: create an empty commit (no changes),
/// then make edits and commit. Tests that git-ai handles commits with no
/// file changes gracefully (shouldn't create a note, shouldn't corrupt state).
#[allow(clippy::too_many_arguments)]
pub fn execute_empty_commit_interleave(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("empty-commit: creating empty commit".to_string());

    // Create an empty commit
    repo.git(&["commit", "--allow-empty", "-m", "empty commit"])
        .unwrap();

    // Now make real edits and commit
    let edit_count = rng.random_range(1..=4);
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("real commit after empty").unwrap();

    operation_log.push("empty-commit: done".to_string());
}

/// Multiple amend with attribution flip: make an AI edit, commit, then amend
/// with a human edit that overwrites the same lines. Tests that the final
/// attribution (human) wins after the amend.
#[allow(clippy::too_many_arguments)]
pub fn execute_amend_attribution_flip(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("amend-flip: starting".to_string());

    // First: AI edit and commit
    let ai_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &ai_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("ai commit to be amended").unwrap();

    // Amend with human edit (overwrite everything)
    let human_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::OverwriteAll,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &human_params,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "amended: AI replaced by human"])
        .unwrap();

    operation_log.push("amend-flip: done (AI -> human overwrite via amend)".to_string());
}

/// Concurrent file creation: create multiple new files simultaneously with
/// different attributions, commit them all at once. Tests multi-file working
/// log handling when many files appear in a single commit.
#[allow(clippy::too_many_arguments)]
pub fn execute_concurrent_file_creation(
    repo: &TestRepo,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) -> Vec<FileState> {
    let file_count = rng.random_range(3..=6);
    operation_log.push(format!(
        "concurrent-create: creating {} files simultaneously",
        file_count
    ));

    let mut new_files = Vec::new();
    let idx_base = registry.next_index();

    for i in 0..file_count {
        let filename = format!("concurrent_{}_{}.txt", idx_base, i);
        let mut fs = FileState::new(&filename);

        let edit_count = rng.random_range(1..=3);
        for _ in 0..edit_count {
            let params = EditParams {
                attribution: gen_attribution(rng),
                strategy: EditStrategy::Append,
                line_count: gen_line_count(rng, max_lines),
            };
            execute_edit_and_checkpoint(repo, &mut fs, registry, &params, rng, operation_log);
        }
        new_files.push(fs);
    }

    // Single commit with all files
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("concurrent file creation").unwrap();

    operation_log.push(format!(
        "concurrent-create: done ({} files committed)",
        file_count
    ));

    new_files
}

/// Stash with partial files: edit multiple files, stash only one using pathspec,
/// commit the others, then pop the stash. Tests selective stash attribution.
#[allow(clippy::too_many_arguments)]
pub fn execute_stash_pathspec(
    repo: &TestRepo,
    file_state_a: &mut FileState,
    file_state_b: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("stash-pathspec: starting".to_string());

    // Edit both files
    let params_a = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state_a, registry, &params_a, rng, operation_log);

    let pre_stash_b = file_state_b.lines.clone();
    let params_b = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state_b, registry, &params_b, rng, operation_log);

    // Stash only file B using pathspec
    let stash_result = repo.git(&[
        "stash",
        "push",
        "-m",
        "stash B only",
        "--",
        &file_state_b.filename,
    ]);
    if stash_result.is_err() {
        operation_log.push("stash-pathspec: stash push failed, skipping".to_string());
        return;
    }

    // File B should be reverted, file A still dirty
    file_state_b.lines = pre_stash_b;

    // Commit file A
    repo.git(&["add", &file_state_a.filename]).unwrap();
    repo.commit("commit A while B is stashed").unwrap();

    // Pop stash (restores file B)
    let pop_result = repo.git(&["stash", "pop"]);
    if pop_result.is_err() {
        repo.git(&["stash", "drop"]).ok();
        operation_log.push("stash-pathspec: pop failed, dropped".to_string());
        return;
    }

    // Re-read file B from disk
    let path_b = repo.path().join(&file_state_b.filename);
    if path_b.exists() {
        let content = fs::read_to_string(&path_b).unwrap();
        file_state_b.lines = reconstruct_lines_from_content(&content);
    }

    // Commit file B
    repo.git(&["add", &file_state_b.filename]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("commit B after stash pop").unwrap();
    }

    operation_log.push("stash-pathspec: done".to_string());
}

/// Rebase with multiple commits that touch the same lines.
/// Creates a branch with 3+ commits each modifying the same region,
/// then rebases onto a diverged main. Extremely pathological for attribution
/// because each rebase step must correctly remap line numbers.
#[allow(clippy::too_many_arguments)]
pub fn execute_multi_commit_rebase(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let branch_name = format!("multi-rebase-{}", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!("multi-commit-rebase: start branch={}", branch_name));

    // Ensure we have committed state
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("pre-multi-rebase commit").unwrap();
    }

    let pre_len = file_state.lines.len();

    // Create feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make 3-5 commits on the branch, each appending
    let commit_count = rng.random_range(3..=5u32);
    for i in 0..commit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(4)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("multi-rebase feature commit {}", i + 1))
            .unwrap();
    }
    let feature_lines = file_state.lines.clone();

    // Switch to main and advance it with prepends
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = file_state.lines[..pre_len].to_vec();

    let main_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    file_state.lines = reconstruct_lines_from_content(&main_content);

    let main_edits = rng.random_range(1..=2);
    for _ in 0..main_edits {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Prepend,
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("advance main for multi-rebase").unwrap();
    let main_new_lines = file_state.lines.clone();

    // Rebase feature onto main
    repo.git(&["checkout", &branch_name]).unwrap();
    let rebase_result = repo.git(&["rebase", &main_branch]);
    if rebase_result.is_err() {
        // Abort on conflict
        repo.git(&["rebase", "--abort"]).ok();
        repo.git(&["checkout", &main_branch]).unwrap();
        repo.git(&["branch", "-D", &branch_name]).unwrap();
        operation_log.push("multi-commit-rebase: aborted due to conflict".to_string());
        return;
    }

    // After rebase: main's prepended lines + original + feature's appended
    let feature_appended: Vec<char> = feature_lines[pre_len..].to_vec();
    let mut expected = main_new_lines;
    expected.extend(feature_appended);
    file_state.lines = expected;

    // Merge back to main (fast-forward)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", &branch_name]).unwrap();
    repo.git(&["branch", "-d", &branch_name]).unwrap();

    // Verify disk
    let actual_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    let actual_lines = reconstruct_lines_from_content(&actual_content);
    if file_state.lines != actual_lines {
        operation_log.push(format!(
            "multi-commit-rebase: model diverged (model={} disk={}), trusting disk",
            file_state.lines.len(),
            actual_lines.len()
        ));
        file_state.lines = actual_lines;
    }

    operation_log.push("multi-commit-rebase: done".to_string());
}

/// Alternating amend cycle: make AI commit, amend with human, amend back with AI.
/// Each amend completely changes the attribution. Tests that the daemon correctly
/// tracks the final state after multiple attribution flips on the same commit.
#[allow(clippy::too_many_arguments)]
pub fn execute_alternating_amend(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let flip_count = rng.random_range(3..=6);
    operation_log.push(format!("alternating-amend: {} flips starting", flip_count));

    // Initial commit
    let first_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &first_params,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("alternating amend base").unwrap();

    // Alternate between AI and human amends
    for i in 0..flip_count {
        let attr = if i % 2 == 0 {
            Attribution::KnownHuman
        } else {
            Attribution::Ai
        };
        let params = EditParams {
            attribution: attr,
            strategy: EditStrategy::OverwriteAll,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.git(&[
            "commit",
            "--amend",
            "-m",
            &format!("alternating amend flip {}", i),
        ])
        .unwrap();
    }

    operation_log.push("alternating-amend: done".to_string());
}

/// Squash merge with partial staging on the target: squash merge a branch,
/// but only stage part of the result before committing.
#[allow(clippy::too_many_arguments)]
pub fn execute_squash_partial_stage(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let branch_name = format!("squash-partial-{}", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!("squash-partial: start branch={}", branch_name));

    // Ensure clean state
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("pre-squash-partial commit").unwrap();
    }

    let pre_squash_lines = file_state.lines.clone();

    // Create feature branch with edits
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    let commit_count = rng.random_range(2..=3u32);
    for i in 0..commit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(4)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("squash-partial branch commit {}", i + 1))
            .unwrap();
    }
    let all_new_lines = file_state.lines.clone();
    let new_line_count = all_new_lines.len() - pre_squash_lines.len();

    // Switch back to main
    repo.git(&["checkout", &main_branch]).unwrap();
    file_state.lines = pre_squash_lines.clone();

    // Squash merge (puts changes in index)
    repo.git(&["merge", "--squash", &branch_name]).unwrap();

    if new_line_count >= 2 {
        // Partially unstage: reset the file, write only partial content, re-add
        let lines_to_keep = rng.random_range(1..new_line_count);
        let partial_lines: Vec<char> = pre_squash_lines
            .iter()
            .chain(
                all_new_lines[pre_squash_lines.len()..pre_squash_lines.len() + lines_to_keep]
                    .iter(),
            )
            .copied()
            .collect();

        // Write partial version
        let partial_state = FileState {
            lines: partial_lines.clone(),
            filename: file_state.filename.clone(),
        };
        partial_state.write_to_disk(repo);
        repo.git(&["add", &file_state.filename]).unwrap();
        repo.commit("squash-partial: partial commit").unwrap();

        file_state.lines = partial_lines;

        operation_log.push(format!(
            "squash-partial: committed {}/{} new lines from squash",
            lines_to_keep, new_line_count
        ));
    } else {
        // Too few lines, just commit everything
        repo.git(&["commit", "-m", "squash-partial: full commit"])
            .unwrap();
        file_state.lines = all_new_lines;
    }

    // Cleanup
    repo.git(&["branch", "-D", &branch_name]).unwrap();
    operation_log.push("squash-partial: done".to_string());
}

/// Checkpoint without subsequent commit: fire checkpoints, then hard-reset.
/// This creates orphaned working log entries that the daemon must handle gracefully.
#[allow(clippy::too_many_arguments)]
pub fn execute_orphaned_checkpoints(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("orphaned-checkpoints: starting".to_string());

    // Make several edits with checkpoints
    let orphan_count = rng.random_range(3..=8);
    for _ in 0..orphan_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random(rng)
            },
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    // Now THROW IT ALL AWAY with hard reset
    repo.git(&["checkout", "--", "."]).unwrap();

    // Re-read from disk
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        let content = fs::read_to_string(&path).unwrap();
        file_state.lines = reconstruct_lines_from_content(&content);
    } else {
        file_state.lines.clear();
    }

    operation_log.push(format!(
        "orphaned-checkpoints: discarded {} edits, file has {} lines",
        orphan_count,
        file_state.lines.len()
    ));
}

/// Double-commit rapid fire: make edits, commit, immediately make more edits
/// and commit again without pausing. Tests that the daemon correctly processes
/// back-to-back commits with no breathing room.
#[allow(clippy::too_many_arguments)]
pub fn execute_double_commit_rapid(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let pair_count = rng.random_range(2..=4);
    operation_log.push(format!(
        "double-commit-rapid: {} rapid commit pairs",
        pair_count
    ));

    for i in 0..pair_count {
        // First commit
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
        repo.commit(&format!("rapid pair {} commit 1", i)).unwrap();

        // Immediately second commit
        let params2 = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params2, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("rapid pair {} commit 2", i)).unwrap();
    }

    operation_log.push("double-commit-rapid: done".to_string());
}

/// Thrash operation: rapidly alternate between editing, committing, resetting,
/// and re-editing on the same file. Creates maximum chaos for the daemon's
/// working log management.
#[allow(clippy::too_many_arguments)]
pub fn execute_thrash(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let thrash_cycles = rng.random_range(2..=5);
    operation_log.push(format!("thrash: {} cycles starting", thrash_cycles));

    for cycle in 0..thrash_cycles {
        // Step 1: Make edits and checkpoint
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

        // Step 2: Commit
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("thrash cycle {} commit", cycle))
            .unwrap();

        // Step 3: Immediately make more edits
        let params2 = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params2, rng, operation_log);

        // Step 4: Sometimes discard, sometimes amend, sometimes commit fresh
        match rng.random_range(0..3u32) {
            0 => {
                // Discard and reset
                repo.git(&["checkout", "--", "."]).unwrap();
                let path = repo.path().join(&file_state.filename);
                if path.exists() {
                    let content = fs::read_to_string(&path).unwrap();
                    file_state.lines = reconstruct_lines_from_content(&content);
                }
                operation_log.push(format!("thrash cycle {}: discarded", cycle));
            }
            1 => {
                // Amend into previous
                repo.git(&["add", "-A"]).unwrap();
                repo.git(&[
                    "commit",
                    "--amend",
                    "-m",
                    &format!("thrash cycle {} amended", cycle),
                ])
                .unwrap();
                operation_log.push(format!("thrash cycle {}: amended", cycle));
            }
            _ => {
                // Fresh commit
                repo.git(&["add", "-A"]).unwrap();
                repo.commit(&format!("thrash cycle {} extra commit", cycle))
                    .unwrap();
                operation_log.push(format!("thrash cycle {}: extra commit", cycle));
            }
        }
    }

    // Final state sync
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        let content = fs::read_to_string(&path).unwrap();
        file_state.lines = reconstruct_lines_from_content(&content);
    }

    operation_log.push("thrash: done".to_string());
}

/// Rebase-then-amend: rebase a branch, then immediately amend the rebased commits.
/// This is one of the most pathological operations because it combines two
/// history-rewriting operations back-to-back.
#[allow(clippy::too_many_arguments)]
pub fn execute_rebase_then_amend(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let branch_name = format!("rebase-amend-{}", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!("rebase-then-amend: start branch={}", branch_name));

    // Ensure clean state
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("pre-rebase-amend commit").unwrap();
    }

    // Create feature branch with appends
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    let params_feature = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &params_feature,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rebase-amend feature commit").unwrap();

    // Advance main with prepends
    repo.git(&["checkout", &main_branch]).unwrap();
    let main_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    file_state.lines = reconstruct_lines_from_content(&main_content);

    let params_main = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params_main, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("advance main for rebase-amend").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", &branch_name]).unwrap();
    let rebase_result = repo.git(&["rebase", &main_branch]);
    if rebase_result.is_err() {
        repo.git(&["rebase", "--abort"]).ok();
        repo.git(&["checkout", &main_branch]).unwrap();
        repo.git(&["branch", "-D", &branch_name]).unwrap();
        operation_log.push("rebase-then-amend: aborted (conflict)".to_string());
        return;
    }

    // Now AMEND the rebased commit with new content
    let params_amend = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &params_amend,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&[
        "commit",
        "--amend",
        "-m",
        "rebase-amend: amended after rebase",
    ])
    .unwrap();

    // Read actual state from disk (rebase + amend makes model complex)
    let actual_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    file_state.lines = reconstruct_lines_from_content(&actual_content);

    // Merge back to main
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", &branch_name]).unwrap();
    repo.git(&["branch", "-d", &branch_name]).unwrap();

    // Re-sync from disk
    let final_content = fs::read_to_string(repo.path().join(&file_state.filename)).unwrap();
    file_state.lines = reconstruct_lines_from_content(&final_content);

    operation_log.push("rebase-then-amend: done".to_string());
}

/// Checkpoint on non-existent file: fire a checkpoint on a file that doesn't
/// exist yet, then create it. Tests daemon resilience to out-of-order operations.
#[allow(clippy::too_many_arguments)]
pub fn execute_checkpoint_nonexistent(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let ghost_filename = format!("ghost_{}.txt", idx);

    operation_log.push(format!(
        "checkpoint-nonexistent: firing checkpoint on '{}'",
        ghost_filename
    ));

    // Fire checkpoint on a file that doesn't exist
    repo.git_ai(&["checkpoint", "human", &ghost_filename]).ok();
    repo.git_ai(&["checkpoint", "mock_ai", &ghost_filename])
        .ok();

    // Now actually create the file with real content
    let mut ghost_state = FileState::new(&ghost_filename);
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(
        repo,
        &mut ghost_state,
        registry,
        &params,
        rng,
        operation_log,
    );

    // Also make an edit to the main file
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
    repo.commit("commit after ghost checkpoint").unwrap();

    operation_log.push("checkpoint-nonexistent: done".to_string());
}

/// Interleaved branch commits: create two branches from the same point,
/// make different edits on each, merge one, then merge the other (non-ff).
/// Tests attribution through merge commits with actual merge parents.
#[allow(clippy::too_many_arguments)]
pub fn execute_two_branch_merge(
    repo: &TestRepo,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
    seed: u64,
) {
    let idx = registry.next_index();
    let branch_a = format!("merge-a-{}", idx);
    let branch_b = format!("merge-b-{}", idx + 1);
    let main_branch = repo.current_branch();

    operation_log.push(format!(
        "two-branch-merge: branches={}, {}",
        branch_a, branch_b
    ));

    // Ensure clean state
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("pre-two-branch commit").unwrap();
    }

    // Create two separate files, one per branch, to avoid conflicts
    let file_a_name = format!("branch_a_{}.txt", idx);
    let file_b_name = format!("branch_b_{}.txt", idx);

    // Branch A: create file_a with AI edits
    repo.git(&["checkout", "-b", &branch_a]).unwrap();
    let mut file_a = FileState::new(&file_a_name);
    let params_a = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, &mut file_a, registry, &params_a, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("branch A commit").unwrap();

    // Back to main, create branch B
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["checkout", "-b", &branch_b]).unwrap();
    let mut file_b = FileState::new(&file_b_name);
    let params_b = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, &mut file_b, registry, &params_b, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("branch B commit").unwrap();

    // Back to main, merge A (fast-forward)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", &branch_a]).unwrap();

    // Now merge B (creates a real merge commit since main advanced)
    let merge_result = repo.git(&["merge", &branch_b, "-m", "merge branch B"]);
    if merge_result.is_err() {
        // Conflict: abort
        repo.git(&["merge", "--abort"]).ok();
        repo.git(&["branch", "-D", &branch_a]).ok();
        repo.git(&["branch", "-D", &branch_b]).ok();
        operation_log.push("two-branch-merge: conflict, aborted".to_string());
        return;
    }

    // Verify both files
    registry.verify_blame(repo, &file_a_name, &file_a.lines, operation_log, seed);
    registry.verify_blame(repo, &file_b_name, &file_b.lines, operation_log, seed);

    // Cleanup
    repo.git(&["branch", "-d", &branch_a]).ok();
    repo.git(&["branch", "-d", &branch_b]).ok();

    operation_log.push("two-branch-merge: done".to_string());
}

/// Rapid successive amends with increasing file size: start with 1 line,
/// amend to 2, amend to 4, amend to 8... doubling each time.
/// Stresses the daemon's working log diff computation on growing files.
#[allow(clippy::too_many_arguments)]
pub fn execute_exponential_amend(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let steps = rng.random_range(3..=5);
    operation_log.push(format!("exponential-amend: {} doubling steps", steps));

    // Initial commit with 1 line
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: 1,
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("exponential amend base").unwrap();

    // Each step: overwrite with double the lines, then amend
    let mut size = 2;
    for _ in 0..steps {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::OverwriteAll,
            line_count: size,
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.git(&[
            "commit",
            "--amend",
            "-m",
            &format!("exponential amend size={}", size),
        ])
        .unwrap();
        size = (size * 2).min(32); // Cap at 32 to avoid excessive test time
    }

    operation_log.push(format!(
        "exponential-amend: done (final size={})",
        file_state.lines.len()
    ));
}

/// Edit the same file from two "sessions" (AI and human) in rapid alternation
/// without any commits in between, then commit once. Each session checkpoints
/// after its edit. Tests that the daemon correctly interleaves attributions.
#[allow(clippy::too_many_arguments)]
pub fn execute_session_interleave(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let interleave_count = rng.random_range(4..=10);
    operation_log.push(format!(
        "session-interleave: {} alternating edits",
        interleave_count
    ));

    for i in 0..interleave_count {
        let attr = if i % 2 == 0 {
            Attribution::Ai
        } else {
            Attribution::KnownHuman
        };
        let strategy = if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            // Mix of appends and prepends for maximum interleaving
            match i % 4 {
                0 => EditStrategy::Append,
                1 => EditStrategy::Prepend,
                2 => EditStrategy::InsertRandom,
                _ => EditStrategy::Append,
            }
        };
        let params = EditParams {
            attribution: attr,
            strategy,
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    // Single commit after all the interleaved edits
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("commit after session interleave").unwrap();

    operation_log.push(format!(
        "session-interleave: done ({} lines)",
        file_state.lines.len()
    ));
}

/// Partial stage then amend: partially stage a file, commit, then immediately
/// amend with the rest. This combines partial staging with amend, one of the most
/// complex interactions for working log management.
#[allow(clippy::too_many_arguments)]
pub fn execute_partial_then_amend(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("partial-then-amend: starting".to_string());

    let pre_edit_lines = file_state.lines.clone();

    // Make several edits
    let edit_count = rng.random_range(3..=5);
    for _ in 0..edit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    let new_line_count = file_state.lines.len() - pre_edit_lines.len();
    if new_line_count < 2 {
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("partial-then-amend: degenerate").unwrap();
        operation_log.push("partial-then-amend: degenerate (too few lines)".to_string());
        return;
    }

    // Stage first half
    let half = new_line_count / 2;
    let partial_lines: Vec<char> = pre_edit_lines
        .iter()
        .chain(file_state.lines[pre_edit_lines.len()..pre_edit_lines.len() + half].iter())
        .copied()
        .collect();

    let full_lines = file_state.lines.clone();

    // Write partial, stage it
    let partial_state = FileState {
        lines: partial_lines,
        filename: file_state.filename.clone(),
    };
    partial_state.write_to_disk(repo);
    repo.git(&["add", &file_state.filename]).unwrap();

    // Write full back
    file_state.write_to_disk(repo);

    // Commit (only partial is staged)
    repo.commit("partial-then-amend: partial commit").unwrap();

    // Now immediately amend with the full content
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&[
        "commit",
        "--amend",
        "-m",
        "partial-then-amend: amended with full content",
    ])
    .unwrap();

    file_state.lines = full_lines;
    operation_log.push("partial-then-amend: done".to_string());
}

/// Stash during rebase: start a rebase, then stash uncommitted changes mid-way.
/// This simulates a user who gets interrupted during a rebase.
/// We approximate this by: creating a branch, making edits, starting rebase,
/// and if it succeeds, making more edits + stash + pop before merging back.
#[allow(clippy::too_many_arguments)]
pub fn execute_stash_during_work(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("stash-during-work: starting".to_string());

    // Make some edits with checkpoints
    let edit_count = rng.random_range(2..=4);
    for _ in 0..edit_count {
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
    }

    // Stash everything
    let stash_result = repo.git(&["stash", "push", "-m", "mid-work stash"]);
    if stash_result.is_err() {
        // Nothing to stash (no changes from committed state)
        operation_log.push("stash-during-work: nothing to stash, committing directly".to_string());
        repo.git(&["add", "-A"]).unwrap();
        let status = repo.git(&["status", "--porcelain"]).unwrap();
        if !status.trim().is_empty() {
            repo.commit("stash-during-work: direct commit").unwrap();
        }
        return;
    }

    // Re-read file state from disk (stash reverts to committed state)
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        let content = fs::read_to_string(&path).unwrap();
        file_state.lines = reconstruct_lines_from_content(&content);
    }

    // Make a DIFFERENT edit and commit while stash is active
    let interim_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &interim_params,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("stash-during-work: interim commit").unwrap();

    // Pop stash
    let pop_result = repo.git(&["stash", "pop"]);
    if pop_result.is_err() {
        // Conflict: drop the stash
        repo.git(&["checkout", "--", "."]).ok();
        repo.git(&["stash", "drop"]).ok();
        operation_log.push("stash-during-work: pop conflict, dropped stash".to_string());
        // Re-read from disk
        let path = repo.path().join(&file_state.filename);
        if path.exists() {
            let content = fs::read_to_string(&path).unwrap();
            file_state.lines = reconstruct_lines_from_content(&content);
        }
        return;
    }

    // After pop: re-read from disk to get merged state
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        let content = fs::read_to_string(&path).unwrap();
        file_state.lines = reconstruct_lines_from_content(&content);
    }

    // Commit the popped changes
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("stash-during-work: after pop commit").unwrap();
    }

    operation_log.push("stash-during-work: done".to_string());
}

/// Cross-file checkpoint race: fire checkpoints on multiple files in rapid
/// succession, interleaving AI and human checkpoints across files, then
/// commit everything at once. Stresses the daemon's per-file sequencer.
#[allow(clippy::too_many_arguments)]
pub fn execute_cross_file_checkpoint_race(
    repo: &TestRepo,
    file_states: &mut [&mut FileState],
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let rounds = rng.random_range(3..=8);
    operation_log.push(format!(
        "cross-file-race: {} rounds across {} files",
        rounds,
        file_states.len()
    ));

    for _ in 0..rounds {
        // Pick a random file and make an edit
        let file_idx = rng.random_range(0..file_states.len());
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_states[file_idx].lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(
            repo,
            file_states[file_idx],
            registry,
            &params,
            rng,
            operation_log,
        );
    }

    // Single commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cross-file checkpoint race commit").unwrap();

    operation_log.push("cross-file-race: done".to_string());
}

/// Commit with only whitespace changes alongside attributed changes.
/// The whitespace-only file should not interfere with attribution of other files.
#[allow(clippy::too_many_arguments)]
pub fn execute_whitespace_noise(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("whitespace-noise: starting".to_string());

    let idx = registry.next_index();
    let noise_file = format!("whitespace_noise_{}.txt", idx);

    // Create a file with just whitespace/newlines (no meaningful content)
    let noise_content = "\n\n   \n\t\n   \n\n";
    fs::write(repo.path().join(&noise_file), noise_content).unwrap();

    // Make a real attributed edit to the main file
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

    // Commit both together
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("commit with whitespace noise file").unwrap();

    operation_log.push("whitespace-noise: done".to_string());
}

/// Multiple sequential amend-then-reset cycles: amend a commit, then soft-reset,
/// then recommit, then amend again. This creates maximum confusion for working logs.
#[allow(clippy::too_many_arguments)]
pub fn execute_amend_reset_cycle(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let cycles = rng.random_range(2..=4);
    operation_log.push(format!("amend-reset-cycle: {} cycles", cycles));

    // Initial commit
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("amend-reset-cycle base").unwrap();

    for i in 0..cycles {
        // Amend with new content
        let amend_params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(
            repo,
            file_state,
            registry,
            &amend_params,
            rng,
            operation_log,
        );
        repo.git(&["add", "-A"]).unwrap();
        repo.git(&[
            "commit",
            "--amend",
            "-m",
            &format!("amend-reset cycle {} amend", i),
        ])
        .unwrap();

        // Soft reset
        repo.git(&["reset", "--soft", "HEAD~1"]).unwrap();

        // Recommit (all changes are staged from soft reset)
        repo.commit(&format!("amend-reset cycle {} recommit", i))
            .unwrap();
    }

    operation_log.push("amend-reset-cycle: done".to_string());
}

/// Cherry-pick with conflicts: create a branch, make conflicting edits on both
/// branches, then cherry-pick from one to the other, aborting if it conflicts.
/// Tests that attribution survives or is correctly invalidated through cherry-pick.
#[allow(clippy::too_many_arguments)]
pub fn execute_cherry_pick_conflict(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("cherry-pick-conflict: starting".to_string());

    let branch_name = format!("cherry-pick-conflict-{}", rng.random_range(0..10000u32));

    // Create and switch to feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make edits on the feature branch
    let feature_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::Prepend
        },
        line_count: gen_line_count(rng, max_lines.min(4)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &feature_params,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cherry-pick-conflict: feature commit").unwrap();

    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to previous branch
    repo.git(&["checkout", "-"]).unwrap();

    // Make a conflicting edit on main (different content at same position)
    let main_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("cherry-pick-conflict: main commit").unwrap();

    // Try cherry-pick
    let cp_result = repo.git(&["cherry-pick", &feature_sha]);
    if cp_result.is_err() {
        // Conflict - abort
        repo.git(&["cherry-pick", "--abort"]).unwrap();
        operation_log.push("cherry-pick-conflict: conflict, aborted".to_string());
    } else {
        operation_log.push("cherry-pick-conflict: clean apply".to_string());
    }

    // Re-read file state from disk
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Clean up branch
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("cherry-pick-conflict: done".to_string());
}

/// Rapid branch create-commit-merge cycles: create multiple short-lived branches,
/// each with a single commit, and merge them all back. Tests merge attribution
/// under rapid succession.
#[allow(clippy::too_many_arguments)]
pub fn execute_rapid_branch_merge(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let branch_count = rng.random_range(2..=4);
    operation_log.push(format!("rapid-branch-merge: {} branches", branch_count));

    let base_branch = repo
        .git(&["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    for i in 0..branch_count {
        let branch_name = format!("rapid-merge-{}-{}", rng.random_range(0..10000u32), i);

        // Create branch from current HEAD
        repo.git(&["checkout", "-b", &branch_name]).unwrap();

        // Make an edit (append only to avoid conflicts)
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("rapid-branch-merge: branch {} commit", i))
            .unwrap();

        // Merge back immediately
        repo.git(&["checkout", &base_branch]).unwrap();
        let merge_result = repo.git(&["merge", &branch_name, "--no-edit"]);
        if merge_result.is_err() {
            // Conflict - just abort and move on
            repo.git(&["merge", "--abort"]).ok();
            operation_log.push(format!("rapid-branch-merge: branch {} conflict", i));
        }

        // Clean up
        repo.git(&["branch", "-D", &branch_name]).ok();

        // Re-read state
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
    }

    operation_log.push("rapid-branch-merge: done".to_string());
}

/// Interleaved rebase and cherry-pick: rebase some commits, then cherry-pick
/// others from the pre-rebase history. Maximum confusion for authorship tracking.
#[allow(clippy::too_many_arguments)]
pub fn execute_rebase_cherry_pick_combo(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("rebase-cherry-pick-combo: starting".to_string());

    // Make 3 commits on a branch
    let branch_name = format!("rebase-cp-{}", rng.random_range(0..10000u32));
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    let mut branch_shas = Vec::new();
    for i in 0..3 {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("rebase-cp branch commit {}", i))
            .unwrap();
        branch_shas.push(repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string());
    }

    // Switch back to base
    repo.git(&["checkout", "-"]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Make a commit on the base to create divergence
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rebase-cp: base divergence commit").unwrap();

    // Try cherry-pick of first branch commit
    let cp_result = repo.git(&["cherry-pick", &branch_shas[0]]);
    if cp_result.is_err() {
        repo.git(&["cherry-pick", "--abort"]).ok();
        operation_log.push("rebase-cherry-pick-combo: cherry-pick failed, skipping".to_string());
    } else {
        operation_log.push("rebase-cherry-pick-combo: cherry-pick succeeded".to_string());
    }

    // Re-read state
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Clean up branch
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("rebase-cherry-pick-combo: done".to_string());
}

/// Commit, then immediately `git reset --mixed HEAD~1`, edit more, re-commit.
/// This simulates "oops, forgot something" which is extremely common.
#[allow(clippy::too_many_arguments)]
pub fn execute_reset_edit_recommit(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("reset-edit-recommit: starting".to_string());

    // First make an edit and commit
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("reset-edit-recommit: initial").unwrap();

    // Mixed reset (changes go back to working tree)
    repo.git(&["reset", "--mixed", "HEAD~1"]).unwrap();

    // Make MORE edits on top
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

    // Recommit everything
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("reset-edit-recommit: recommitted with extra")
        .unwrap();

    operation_log.push("reset-edit-recommit: done".to_string());
}

/// Multiple sequential checkpoints without any git operations between them.
/// The daemon should handle the rapid-fire checkpoints on the same file
/// without losing or duplicating data.
#[allow(clippy::too_many_arguments)]
pub fn execute_checkpoint_storm(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let storm_count = rng.random_range(5..=15);
    operation_log.push(format!("checkpoint-storm: {} checkpoints", storm_count));

    for i in 0..storm_count {
        let attr = if i % 3 == 0 {
            Attribution::KnownHuman
        } else {
            Attribution::Ai
        };
        let strategy = if file_state.lines.is_empty() || i % 4 == 0 {
            EditStrategy::Append
        } else {
            match i % 5 {
                0 => EditStrategy::Append,
                1 => EditStrategy::Prepend,
                2 => EditStrategy::InsertRandom,
                3 => EditStrategy::ReplaceRandom,
                _ => EditStrategy::Append,
            }
        };
        let params = EditParams {
            attribution: attr,
            strategy,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    // Single commit after the storm
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("checkpoint storm commit").unwrap();

    operation_log.push(format!(
        "checkpoint-storm: done ({} lines)",
        file_state.lines.len()
    ));
}

/// Partial stage with amend: stage only SOME lines, commit, then amend
/// with different attribution lines. Tests that amend correctly handles
/// the working log split between committed and uncommitted.
#[allow(clippy::too_many_arguments)]
pub fn execute_partial_amend_flip(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("partial-amend-flip: starting".to_string());

    // Make AI edits (append)
    let ai_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(4)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &ai_params, rng, operation_log);

    // Make human edits (also append)
    let human_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(4)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &human_params,
        rng,
        operation_log,
    );

    // Commit all
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("partial-amend-flip: initial").unwrap();

    // Now amend with the OPPOSITE attribution
    let flip_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &flip_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "partial-amend-flip: amended"])
        .unwrap();

    operation_log.push("partial-amend-flip: done".to_string());
}

/// Make edits, checkpoint, then `git checkout -- file` to discard, then
/// make NEW edits with different attribution and commit. The discarded
/// checkpoints should not appear in the final authorship.
#[allow(clippy::too_many_arguments)]
pub fn execute_discard_then_reedit(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("discard-then-reedit: starting".to_string());

    // Make some AI edits
    let ai_params = EditParams {
        attribution: Attribution::Ai,
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &ai_params, rng, operation_log);

    // Discard all changes
    repo.git(&["checkout", "--", &file_state.filename]).ok();

    // Re-read from disk (back to committed state)
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Make NEW edits with human attribution
    let human_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &human_params,
        rng,
        operation_log,
    );

    // Commit
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("discard-then-reedit: commit").unwrap();
    }

    operation_log.push("discard-then-reedit: done".to_string());
}

/// Create multiple files, checkpoint them all with different attributions,
/// then delete some and commit. Tests that deletion doesn't corrupt
/// attribution of remaining files.
#[allow(clippy::too_many_arguments)]
pub fn execute_create_delete_batch(
    repo: &TestRepo,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) -> Vec<FileState> {
    let file_count = rng.random_range(3..=5);
    operation_log.push(format!("create-delete-batch: {} files", file_count));

    let mut batch_files: Vec<FileState> = Vec::new();
    let base_idx = registry.next_index();

    // Create all files with different attributions
    for i in 0..file_count {
        let filename = format!("batch_{}_{}.txt", base_idx, i);
        let mut fs = FileState::new(&filename);
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(4)),
        };
        execute_edit_and_checkpoint(repo, &mut fs, registry, &params, rng, operation_log);
        batch_files.push(fs);
    }

    // Commit all
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("create-delete-batch: all files").unwrap();

    // Delete a random subset
    let delete_count = rng.random_range(1..file_count);
    let mut kept_files = Vec::new();
    for (i, fs) in batch_files.into_iter().enumerate() {
        if i < delete_count {
            let path = repo.path().join(&fs.filename);
            std::fs::remove_file(&path).ok();
            operation_log.push(format!("create-delete-batch: deleted {}", fs.filename));
        } else {
            kept_files.push(fs);
        }
    }

    // Commit the deletions
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("create-delete-batch: deletions").unwrap();
    }

    operation_log.push("create-delete-batch: done".to_string());
    kept_files
}

/// Simulate an interactive rebase (squash): make N commits, then squash them all
/// into one. All attribution from all N commits should appear in the squashed result.
#[allow(clippy::too_many_arguments)]
pub fn execute_multi_squash(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let commit_count = rng.random_range(3..=6);
    operation_log.push(format!("multi-squash: {} commits", commit_count));

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    for i in 0..commit_count {
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
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("multi-squash commit {}", i)).unwrap();
    }

    // Squash all commits since base into one
    repo.git(&["reset", "--soft", &base_sha]).unwrap();
    repo.commit("multi-squash: squashed all").unwrap();

    operation_log.push("multi-squash: done".to_string());
}

/// Back-to-back amends with alternating AI/human: make a commit, then amend
/// it N times, alternating between AI and human attribution. The final
/// authorship should reflect the cumulative state.
#[allow(clippy::too_many_arguments)]
pub fn execute_alternating_amend_storm(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let amend_count = rng.random_range(4..=10);
    operation_log.push(format!("alternating-amend-storm: {} amends", amend_count));

    // Initial commit
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("alternating-amend-storm: base").unwrap();

    // Rapid amends
    for i in 0..amend_count {
        let attr = if i % 2 == 0 {
            Attribution::Ai
        } else {
            Attribution::KnownHuman
        };
        let amend_params = EditParams {
            attribution: attr,
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(
            repo,
            file_state,
            registry,
            &amend_params,
            rng,
            operation_log,
        );
        repo.git(&["add", "-A"]).unwrap();
        repo.git(&[
            "commit",
            "--amend",
            "-m",
            &format!("alternating-amend-storm: amend {}", i),
        ])
        .unwrap();
    }

    operation_log.push("alternating-amend-storm: done".to_string());
}

/// Rename chain: rename a file through multiple names (A→B→C→D) with edits
/// between each rename. Tests that git's rename detection and authorship
/// tracking survive sequential renames.
#[allow(clippy::too_many_arguments)]
pub fn execute_rename_chain(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let chain_len = rng.random_range(2..=4);
    operation_log.push(format!("rename-chain: {} renames", chain_len));

    for i in 0..chain_len {
        // Edit between renames
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
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("rename-chain: pre-rename {}", i))
            .unwrap();

        // Rename
        let new_name = format!(
            "renamed_{}_{}.txt",
            registry.next_index(),
            rng.random_range(0..1000u32)
        );
        repo.git(&["mv", &file_state.filename, &new_name]).unwrap();
        file_state.filename = new_name;
        repo.commit(&format!("rename-chain: rename {}", i)).unwrap();
    }

    operation_log.push(format!("rename-chain: final name={}", file_state.filename));
}

/// Fixup-style squash: make N commits, then soft reset and recommit (simulating
/// git rebase --autosquash with fixup commits). Each commit has different
/// attribution that should be preserved in the final squashed commit.
#[allow(clippy::too_many_arguments)]
pub fn execute_fixup_squash(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let fixup_count = rng.random_range(2..=5);
    operation_log.push(format!("fixup-squash: {} fixups", fixup_count));

    let base_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Main commit
    let main_params = EditParams {
        attribution: Attribution::Ai,
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("fixup-squash: main commit").unwrap();

    // Fixup commits (small additions)
    for i in 0..fixup_count {
        let attr = if i % 2 == 0 {
            Attribution::KnownHuman
        } else {
            Attribution::Ai
        };
        let params = EditParams {
            attribution: attr,
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("fixup! fixup-squash: fixup {}", i))
            .unwrap();
    }

    // Squash all into one (simulate rebase --autosquash)
    repo.git(&["reset", "--soft", &base_sha]).unwrap();
    repo.commit("fixup-squash: squashed result").unwrap();

    operation_log.push("fixup-squash: done".to_string());
}

/// Empty tree commit then rebuild: delete ALL tracked files, commit the empty
/// tree, then recreate the file from scratch. Tests that authorship starts
/// fresh after a complete wipe.
#[allow(clippy::too_many_arguments)]
pub fn execute_empty_tree_rebuild(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("empty-tree-rebuild: starting".to_string());

    // Delete the main file
    let path = repo.path().join(&file_state.filename);
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("empty-tree-rebuild: deleted file").unwrap();
    }

    // Recreate with new content
    file_state.lines.clear();
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("empty-tree-rebuild: recreated").unwrap();

    operation_log.push("empty-tree-rebuild: done".to_string());
}

/// Git revert: commit something, then revert it, then add new content.
/// Tests that reverted authorship doesn't linger and new attributions are clean.
#[allow(clippy::too_many_arguments)]
pub fn execute_revert_then_redo(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("revert-then-redo: starting".to_string());

    // Make a commit we'll revert
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("revert-then-redo: to be reverted").unwrap();

    let sha_to_revert = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Revert it
    let revert_result = repo.git(&["revert", "--no-edit", &sha_to_revert]);
    if revert_result.is_err() {
        // Conflict during revert - abort
        repo.git(&["revert", "--abort"]).ok();
        operation_log.push("revert-then-redo: revert conflict, aborted".to_string());
        return;
    }

    // Re-read state from disk (revert undid our changes)
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Make new edits with different attribution
    let redo_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &redo_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("revert-then-redo: new content after revert")
            .unwrap();
    }

    operation_log.push("revert-then-redo: done".to_string());
}

/// Multiple files with selective staging: edit 3+ files but only commit some,
/// leaving others dirty, then commit the rest in a second commit.
/// Tests working log integrity when files are split across commits.
#[allow(clippy::too_many_arguments)]
pub fn execute_selective_multi_file_commit(
    repo: &TestRepo,
    file_states: &mut [&mut FileState],
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let file_count = file_states.len();
    operation_log.push(format!("selective-multi-file: {} files", file_count));

    // Edit all files
    for fs in file_states.iter_mut() {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if fs.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(4)),
        };
        execute_edit_and_checkpoint(repo, fs, registry, &params, rng, operation_log);
    }

    // Stage only first half
    let first_half = file_count / 2;
    for fs in file_states[..first_half.max(1)].iter() {
        repo.git(&["add", &fs.filename]).unwrap();
    }
    repo.commit("selective-multi-file: first batch").unwrap();

    // Commit the rest
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("selective-multi-file: second batch").unwrap();
    }

    operation_log.push("selective-multi-file: done".to_string());
}

/// Amend with file deletion: commit file, then amend the commit to also delete
/// another file. Tests that amend with mixed add/delete doesn't corrupt attribution.
#[allow(clippy::too_many_arguments)]
pub fn execute_amend_with_deletion(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("amend-with-deletion: starting".to_string());

    // Create a temporary file
    let temp_name = format!("temp_delete_{}.txt", registry.next_index());
    let temp_path = repo.path().join(&temp_name);
    let mut temp_state = FileState::new(&temp_name);
    let temp_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        &mut temp_state,
        registry,
        &temp_params,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("amend-with-deletion: setup temp file").unwrap();

    // Make edits to main file and commit
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("amend-with-deletion: main edit").unwrap();

    // Now delete the temp file and amend
    std::fs::remove_file(&temp_path).ok();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&[
        "commit",
        "--amend",
        "-m",
        "amend-with-deletion: amended with file delete",
    ])
    .unwrap();

    operation_log.push("amend-with-deletion: done".to_string());
}

/// Rapid commit-reset-commit cycle on same content: commit, soft reset,
/// re-commit, soft reset, re-commit. The same content gets committed
/// multiple times. Tests that working log handling is idempotent.
#[allow(clippy::too_many_arguments)]
pub fn execute_recommit_loop(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let loop_count = rng.random_range(3..=6);
    operation_log.push(format!("recommit-loop: {} iterations", loop_count));

    // Make edits
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
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("recommit-loop: initial").unwrap();

    // Repeatedly soft-reset and recommit
    for i in 0..loop_count {
        repo.git(&["reset", "--soft", "HEAD~1"]).unwrap();
        repo.commit(&format!("recommit-loop: iteration {}", i))
            .unwrap();
    }

    operation_log.push("recommit-loop: done".to_string());
}

/// INITIAL attribution carryover: make edits and checkpoint, but DON'T commit.
/// Then make MORE edits in the next "round" and commit. The uncommitted edits
/// from the first round should carry forward as INITIAL attributions and not be
/// lost when the second checkpoint occurs.
#[allow(clippy::too_many_arguments)]
pub fn execute_initial_carryover(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let rounds = rng.random_range(2..=4);
    operation_log.push(format!(
        "initial-carryover: {} rounds without commit",
        rounds
    ));

    // Multiple rounds of edit+checkpoint without committing
    for _ in 0..rounds {
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
    }

    // Finally commit everything
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("initial-carryover: all rounds committed together")
        .unwrap();

    operation_log.push("initial-carryover: done".to_string());
}

/// Merge conflict resolution: create branches with conflicting edits to the
/// same lines, attempt merge, resolve by taking one side, then commit.
/// Tests that attribution after conflict resolution is correct.
#[allow(clippy::too_many_arguments)]
pub fn execute_merge_conflict_resolve(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("merge-conflict-resolve: starting".to_string());

    let branch_name = format!("merge-conflict-{}", rng.random_range(0..10000u32));
    let base_branch = repo
        .git(&["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Create feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make edit on feature branch (append to avoid positional conflicts initially)
    let feature_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &feature_params,
        rng,
        operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("merge-conflict-resolve: feature edit").unwrap();

    // Switch back and make a DIFFERENT append (should merge cleanly)
    repo.git(&["checkout", &base_branch]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    let main_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &main_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("merge-conflict-resolve: main edit").unwrap();

    // Attempt merge
    let merge_result = repo.git(&["merge", &branch_name, "--no-edit"]);
    if merge_result.is_err() {
        // Conflict: resolve by taking ours
        repo.git(&["checkout", "--ours", &file_state.filename]).ok();
        repo.git(&["add", &file_state.filename]).ok();
        let commit_result = repo.git(&["commit", "--no-edit"]);
        if commit_result.is_err() {
            repo.git(&["merge", "--abort"]).ok();
            operation_log.push("merge-conflict-resolve: abort after conflict".to_string());
        } else {
            operation_log.push("merge-conflict-resolve: resolved with --ours".to_string());
        }
    } else {
        operation_log.push("merge-conflict-resolve: clean merge".to_string());
    }

    // Re-read state
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Clean up
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("merge-conflict-resolve: done".to_string());
}

/// Double checkpoint same file: fire two checkpoints in rapid succession on the
/// same file with DIFFERENT attributions. The second should win for the lines
/// it touches. Tests the daemon's ordering guarantees.
#[allow(clippy::too_many_arguments)]
pub fn execute_double_checkpoint_race(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("double-checkpoint-race: starting".to_string());

    // First: AI checkpoint
    let ai_params = EditParams {
        attribution: Attribution::Ai,
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &ai_params, rng, operation_log);

    // Immediately: human checkpoint (overwrites/extends the AI edit)
    let human_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &human_params,
        rng,
        operation_log,
    );

    // Third: another AI checkpoint
    let ai2_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(2)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &ai2_params, rng, operation_log);

    // Commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("double-checkpoint-race: commit").unwrap();

    operation_log.push("double-checkpoint-race: done".to_string());
}

/// Partial hunk staging using `git add -p` simulation: write content that creates
/// multiple diff hunks, then stage only specific hunks. This is the most common
/// source of attribution bugs in real usage.
#[allow(clippy::too_many_arguments)]
pub fn execute_hunk_partial_stage(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("hunk-partial-stage: starting".to_string());

    if file_state.lines.len() < 6 {
        // Need enough lines to create multiple hunks - bootstrap more
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: 8,
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("hunk-partial-stage: bootstrap lines").unwrap();
    }

    // Make edits at the BEGINNING (hunk 1)
    let hunk1_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &hunk1_params,
        rng,
        operation_log,
    );

    // Make edits at the END (hunk 2 - separated by unchanged lines)
    let hunk2_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(
        repo,
        file_state,
        registry,
        &hunk2_params,
        rng,
        operation_log,
    );

    // Stage only the beginning changes (first hunk)
    // We simulate this by writing just the first hunk's state to the index
    let append_count = hunk2_params.line_count;

    // Create a version with only hunk1 applied (prepend applied, append not)
    let partial_lines: Vec<char> =
        file_state.lines[..file_state.lines.len() - append_count].to_vec();
    let partial_state = FileState {
        lines: partial_lines.clone(),
        filename: file_state.filename.clone(),
    };
    partial_state.write_to_disk(repo);
    repo.git(&["add", &file_state.filename]).unwrap();

    // Write full content back
    file_state.write_to_disk(repo);

    // Commit just the first hunk
    repo.commit("hunk-partial-stage: first hunk only").unwrap();

    // Now commit the rest
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("hunk-partial-stage: remaining hunks").unwrap();

    operation_log.push("hunk-partial-stage: done".to_string());
}

/// Interleaved file operations: rename one file while editing another,
/// then commit both in the same commit. Tests that file ops on one file
/// don't corrupt attribution of other files in the same commit.
#[allow(clippy::too_many_arguments)]
pub fn execute_rename_during_edit(
    repo: &TestRepo,
    file_state: &mut FileState,
    secondary: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("rename-during-edit: starting".to_string());

    // Ensure secondary exists
    if secondary.lines.is_empty() {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(4)),
        };
        execute_edit_and_checkpoint(repo, secondary, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("rename-during-edit: bootstrap secondary")
            .unwrap();
    }

    // Edit the main file
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

    // Rename the secondary file
    let new_name = format!("renamed_sec_{}.txt", rng.random_range(0..10000u32));
    repo.git(&["mv", &secondary.filename, &new_name]).unwrap();
    secondary.filename = new_name;

    // Commit both: the edit AND the rename in one commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rename-during-edit: edit + rename in one commit")
        .unwrap();

    operation_log.push("rename-during-edit: done".to_string());
}

/// Overwrite with same content: write the exact same content that's already
/// committed, checkpoint it, then write different content. Tests that
/// no-op diffs don't confuse the daemon.
#[allow(clippy::too_many_arguments)]
pub fn execute_noop_overwrite(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("noop-overwrite: starting".to_string());

    if file_state.lines.is_empty() {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit("noop-overwrite: bootstrap").unwrap();
    }

    // Write the SAME content and checkpoint (should be a no-op for the daemon)
    file_state.write_to_disk(repo);
    let checkpoint_type = if rng.random_range(0..2u32) == 0 {
        "mock_ai"
    } else {
        "mock_known_human"
    };
    repo.git_ai(&["checkpoint", checkpoint_type, &file_state.filename])
        .ok();

    // Now make a REAL edit
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);

    // Commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("noop-overwrite: real edit after noop").unwrap();

    operation_log.push("noop-overwrite: done".to_string());
}

/// Simulate concurrent AI sessions editing the same file: session 1 edits,
/// checkpoint, session 2 edits, checkpoint, then commit. Both sessions'
/// attributions should be preserved.
#[allow(clippy::too_many_arguments)]
pub fn execute_concurrent_sessions(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let session_count = rng.random_range(2..=4);
    operation_log.push(format!("concurrent-sessions: {} sessions", session_count));

    // Each "session" does a pre-checkpoint (human), edit, post-checkpoint (ai/human)
    for i in 0..session_count {
        // Alternate between AI and human sessions
        let attr = if i % 2 == 0 {
            Attribution::Ai
        } else {
            Attribution::KnownHuman
        };
        let params = EditParams {
            attribution: attr,
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                match i % 3 {
                    0 => EditStrategy::Append,
                    1 => EditStrategy::Prepend,
                    _ => EditStrategy::InsertRandom,
                }
            },
            line_count: gen_line_count(rng, max_lines.min(3)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    }

    // Single commit with all sessions' work
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("concurrent-sessions: all sessions committed")
        .unwrap();

    operation_log.push("concurrent-sessions: done".to_string());
}

/// Amend that reduces file size: commit N lines, then amend with fewer lines.
/// The removed lines' attribution should disappear; remaining lines keep theirs.
#[allow(clippy::too_many_arguments)]
pub fn execute_amend_shrink(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("amend-shrink: starting".to_string());

    // Add a bunch of lines
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines).max(4),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("amend-shrink: initial large commit").unwrap();

    // Remove some lines from the end and amend
    let remove_count = rng.random_range(1..=(file_state.lines.len() / 2).max(1));
    let new_len = file_state.lines.len() - remove_count;
    file_state.lines.truncate(new_len);
    file_state.write_to_disk(repo);

    // Checkpoint the shrunk state
    let checkpoint_type = if rng.random_range(0..2u32) == 0 {
        "mock_ai"
    } else {
        "mock_known_human"
    };
    repo.git_ai(&["checkpoint", checkpoint_type, &file_state.filename])
        .ok();

    // Amend
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "amend-shrink: shrunk"])
        .unwrap();

    operation_log.push(format!(
        "amend-shrink: removed {} lines, {} remain",
        remove_count,
        file_state.lines.len()
    ));
}

/// Deep rebase chain: create a branch with N commits, then rebase it onto
/// a diverged main. Each commit in the chain has different attribution.
/// The rebase rewrites ALL commits, so ALL authorship notes must be rewritten.
#[allow(clippy::too_many_arguments)]
pub fn execute_deep_rebase_chain(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let chain_depth = rng.random_range(3..=7);
    operation_log.push(format!("deep-rebase-chain: depth={}", chain_depth));

    let branch_name = format!("deep-rebase-{}", rng.random_range(0..10000u32));

    // Create feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make N commits on the branch
    for i in 0..chain_depth {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("deep-rebase-chain: commit {}", i))
            .unwrap();
    }

    // Go back to base and make a commit to create divergence
    repo.git(&["checkout", "-"]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Use a DIFFERENT file for the base commit to avoid merge conflicts
    let diverge_file = format!("diverge_{}.txt", rng.random_range(0..10000u32));
    fs::write(repo.path().join(&diverge_file), "divergence\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("deep-rebase-chain: base divergence").unwrap();

    // Rebase the branch onto the new base
    repo.git(&["checkout", &branch_name]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    let rebase_result = repo.git(&["rebase", "-"]);
    if rebase_result.is_err() {
        repo.git(&["rebase", "--abort"]).ok();
        operation_log.push("deep-rebase-chain: rebase failed, aborted".to_string());
        repo.git(&["checkout", "-"]).unwrap();
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        repo.git(&["branch", "-D", &branch_name]).ok();
        return;
    }

    // Fast-forward merge the rebased branch
    repo.git(&["checkout", "-"]).unwrap();
    repo.git(&["merge", "--ff-only", &branch_name]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Clean up
    repo.git(&["branch", "-D", &branch_name]).ok();

    operation_log.push("deep-rebase-chain: done".to_string());
}

/// Untracked edits interleaved: make edits WITHOUT any checkpoint, then make
/// checkpointed edits, then commit. The untracked edits should appear as
/// unattributed human (legacy "human" checkpoint behavior).
#[allow(clippy::too_many_arguments)]
pub fn execute_untracked_interleave(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("untracked-interleave: starting".to_string());

    // Make untracked edits (just write to disk, no checkpoint at all).
    // Use '?' which isn't in the registry - the oracle will skip unknown chars
    // during blame verification since they represent untracked/unattributed content.
    let untracked_count = gen_line_count(rng, max_lines.min(3));
    let raw_lines: Vec<char> = (0..untracked_count).map(|_| '?').collect();
    file_state.lines.extend(&raw_lines);
    file_state.write_to_disk(repo);

    // Now fire a "human" checkpoint (untracked/legacy) to capture the untracked state
    repo.git_ai(&["checkpoint", "human", &file_state.filename])
        .ok();

    // Make REAL checkpointed edits
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);

    // Commit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("untracked-interleave: commit").unwrap();

    operation_log.push("untracked-interleave: done".to_string());
}

/// Rapid HEAD changes: make multiple commits in rapid succession, then reset
/// back to the middle one, then make new commits. Tests that working logs
/// keyed by HEAD sha survive HEAD pointer changes.
#[allow(clippy::too_many_arguments)]
pub fn execute_rapid_head_change(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let commit_count = rng.random_range(3..=5);
    operation_log.push(format!(
        "rapid-head-change: {} commits then reset",
        commit_count
    ));

    let start_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let mut shas = vec![start_sha];

    // Make rapid commits
    for i in 0..commit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("rapid-head-change: commit {}", i))
            .unwrap();
        shas.push(repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string());
    }

    // Reset to a middle commit
    let reset_idx = rng.random_range(1..shas.len() - 1);
    repo.git(&["reset", "--hard", &shas[reset_idx]]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Make new commits from the reset point
    let new_params = EditParams {
        attribution: gen_attribution(rng),
        strategy: if file_state.lines.is_empty() {
            EditStrategy::Append
        } else {
            EditStrategy::random_non_destructive(rng)
        },
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &new_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rapid-head-change: new branch commit").unwrap();

    operation_log.push("rapid-head-change: done".to_string());
}

/// Three-way merge: create two branches from same base, each with different
/// attributions, then merge them together. Tests merge commit authorship.
#[allow(clippy::too_many_arguments)]
pub fn execute_three_way_merge(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("three-way-merge: starting".to_string());

    let base_branch = repo
        .git(&["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let branch_a = format!("three-way-a-{}", rng.random_range(0..10000u32));
    let branch_b = format!("three-way-b-{}", rng.random_range(0..10000u32));

    // Branch A: append AI content
    repo.git(&["checkout", "-b", &branch_a]).unwrap();
    let a_params = EditParams {
        attribution: Attribution::Ai,
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &a_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("three-way-merge: branch A").unwrap();

    // Branch B from base: prepend human content (different location to avoid conflict)
    repo.git(&["checkout", &base_branch]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
    repo.git(&["checkout", "-b", &branch_b]).unwrap();
    let b_params = EditParams {
        attribution: Attribution::KnownHuman,
        strategy: EditStrategy::Prepend,
        line_count: gen_line_count(rng, max_lines.min(3)),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &b_params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("three-way-merge: branch B").unwrap();

    // Back to base, merge A first
    repo.git(&["checkout", &base_branch]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    let merge_a = repo.git(&["merge", &branch_a, "--no-edit"]);
    if merge_a.is_err() {
        repo.git(&["merge", "--abort"]).ok();
        repo.git(&["branch", "-D", &branch_a]).ok();
        repo.git(&["branch", "-D", &branch_b]).ok();
        operation_log.push("three-way-merge: merge A failed, aborted".to_string());
        return;
    }
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Merge B
    let merge_b = repo.git(&["merge", &branch_b, "--no-edit"]);
    if merge_b.is_err() {
        repo.git(&["merge", "--abort"]).ok();
        operation_log.push("three-way-merge: merge B failed, aborted".to_string());
    } else {
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        operation_log.push("three-way-merge: both merged".to_string());
    }

    // Clean up
    repo.git(&["branch", "-D", &branch_a]).ok();
    repo.git(&["branch", "-D", &branch_b]).ok();

    operation_log.push("three-way-merge: done".to_string());
}

/// Checkpoint then immediately commit with --allow-empty-message: tests that
/// the post-commit hook fires correctly even with edge-case commit flags.
#[allow(clippy::too_many_arguments)]
pub fn execute_edge_case_commit_flags(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("edge-case-commit-flags: starting".to_string());

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
    repo.git(&["add", "-A"]).unwrap();

    // Use various commit flags that might confuse the post-commit hook
    let flag_choice = rng.random_range(0..3u32);
    match flag_choice {
        0 => {
            repo.git(&["commit", "--allow-empty-message", "-m", ""])
                .unwrap();
        }
        1 => {
            repo.git(&[
                "commit",
                "-m",
                "edge-case: very long message ".repeat(50).trim_end(),
            ])
            .unwrap();
        }
        _ => {
            repo.git(&[
                "commit",
                "-m",
                "edge-case: special chars !@#$%^&*(){}[]|\\:\";<>?,./~`",
            ])
            .unwrap();
        }
    }

    operation_log.push("edge-case-commit-flags: done".to_string());
}

/// Checkpoint→commit→amend→checkpoint→commit rapid cycle: exercises the working
/// log lifecycle in the most compressed time possible.
#[allow(clippy::too_many_arguments)]
pub fn execute_rapid_lifecycle(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let cycles = rng.random_range(3..=6);
    operation_log.push(format!("rapid-lifecycle: {} cycles", cycles));

    for i in 0..cycles {
        // Edit + checkpoint
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: if file_state.lines.is_empty() {
                EditStrategy::Append
            } else {
                EditStrategy::random_non_destructive(rng)
            },
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);

        // Commit
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("rapid-lifecycle: cycle {} commit", i))
            .unwrap();

        // Immediately amend with more content
        let amend_params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(
            repo,
            file_state,
            registry,
            &amend_params,
            rng,
            operation_log,
        );
        repo.git(&["add", "-A"]).unwrap();
        repo.git(&[
            "commit",
            "--amend",
            "-m",
            &format!("rapid-lifecycle: cycle {} amended", i),
        ])
        .unwrap();
    }

    operation_log.push("rapid-lifecycle: done".to_string());
}

/// Stash with multiple entries: create several stash entries, then pop them
/// in random order. Tests that the stash hook correctly handles multiple
/// stash entries and that attribution is preserved through save/pop cycles.
#[allow(clippy::too_many_arguments)]
pub fn execute_multi_stash(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let stash_count = rng.random_range(2..=4);
    operation_log.push(format!("multi-stash: {} stashes", stash_count));

    let mut stashed = 0;

    for i in 0..stash_count {
        // Make an edit
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

        // Stash it
        let result = repo.git(&["stash", "push", "-m", &format!("multi-stash entry {}", i)]);
        if result.is_ok() {
            stashed += 1;
            // After stash, re-read file (reverts to committed state)
            file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
        }
    }

    // Pop all stashes (newest first)
    for _ in 0..stashed {
        let pop_result = repo.git(&["stash", "pop"]);
        if pop_result.is_err() {
            repo.git(&["checkout", "--", "."]).ok();
            repo.git(&["stash", "drop"]).ok();
        }
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
    }

    // Commit whatever state we're in
    repo.git(&["add", "-A"]).unwrap();
    let status = repo.git(&["status", "--porcelain"]).unwrap();
    if !status.trim().is_empty() {
        repo.commit("multi-stash: final commit").unwrap();
    }

    operation_log.push("multi-stash: done".to_string());
}

/// Overwrite-all then partial rollback: write entirely new content (destroying
/// all previous attributions), commit, then soft reset and partially restore.
/// Tests that OverwriteAll strategy + reset correctly handles attribution wipe.
#[allow(clippy::too_many_arguments)]
pub fn execute_overwrite_and_rollback(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    operation_log.push("overwrite-and-rollback: starting".to_string());

    // Complete overwrite
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::OverwriteAll,
        line_count: gen_line_count(rng, max_lines),
    };
    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("overwrite-and-rollback: overwrite").unwrap();

    // Soft reset
    repo.git(&["reset", "--soft", "HEAD~1"]).unwrap();

    // Add more content on top of the overwritten state
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

    // Recommit
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("overwrite-and-rollback: recommit with extra")
        .unwrap();

    operation_log.push("overwrite-and-rollback: done".to_string());
}

/// Cherry-pick chain: make 3 commits, then cherry-pick them one by one
/// onto a different branch. Each cherry-pick should carry its authorship.
#[allow(clippy::too_many_arguments)]
pub fn execute_cherry_pick_chain(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let chain_len = rng.random_range(2..=4);
    operation_log.push(format!("cherry-pick-chain: {} picks", chain_len));

    let base_branch = repo
        .git(&["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let source_branch = format!("cp-source-{}", rng.random_range(0..10000u32));

    // Create source branch with commits
    repo.git(&["checkout", "-b", &source_branch]).unwrap();
    let mut shas = Vec::new();

    for i in 0..chain_len {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("cherry-pick-chain: source {}", i))
            .unwrap();
        shas.push(repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string());
    }

    // Go back to base
    repo.git(&["checkout", &base_branch]).unwrap();
    file_state.lines = read_file_state_from_disk(repo, &file_state.filename);

    // Cherry-pick each commit one by one
    for (i, sha) in shas.iter().enumerate() {
        let cp_result = repo.git(&["cherry-pick", sha]);
        if cp_result.is_err() {
            repo.git(&["cherry-pick", "--abort"]).ok();
            operation_log.push(format!("cherry-pick-chain: pick {} failed", i));
            break;
        }
        file_state.lines = read_file_state_from_disk(repo, &file_state.filename);
    }

    // Clean up
    repo.git(&["branch", "-D", &source_branch]).ok();

    operation_log.push("cherry-pick-chain: done".to_string());
}

/// Interleaved amend and new commits: make a commit, amend it, make a NEW
/// commit, amend THAT one, etc. Tests that amend doesn't bleed into the
/// previous commit's authorship note.
#[allow(clippy::too_many_arguments)]
pub fn execute_interleaved_amend_new(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_lines: usize,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let pair_count = rng.random_range(2..=4);
    operation_log.push(format!(
        "interleaved-amend-new: {} commit+amend pairs",
        pair_count
    ));

    for i in 0..pair_count {
        // New commit
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
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("interleaved-amend-new: new {}", i))
            .unwrap();

        // Immediately amend with different attribution
        let amend_params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, max_lines.min(2)),
        };
        execute_edit_and_checkpoint(
            repo,
            file_state,
            registry,
            &amend_params,
            rng,
            operation_log,
        );
        repo.git(&["add", "-A"]).unwrap();
        repo.git(&[
            "commit",
            "--amend",
            "-m",
            &format!("interleaved-amend-new: amended {}", i),
        ])
        .unwrap();
    }

    operation_log.push("interleaved-amend-new: done".to_string());
}

/// Reconstruct the char-per-line model from actual file content on disk.
pub fn reconstruct_lines_from_content(content: &str) -> Vec<char> {
    content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().next().unwrap())
        .collect()
}

/// Read file from disk and reconstruct its char model.
pub fn read_file_state_from_disk(repo: &TestRepo, filename: &str) -> Vec<char> {
    let path = repo.path().join(filename);
    if !path.exists() {
        return Vec::new();
    }
    let content = fs::read_to_string(&path).unwrap();
    reconstruct_lines_from_content(&content)
}
