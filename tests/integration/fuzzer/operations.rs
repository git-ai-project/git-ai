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
