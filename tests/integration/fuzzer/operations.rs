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
pub fn execute_cherry_pick_same_file(
    repo: &TestRepo,
    _file_state: &mut FileState,
    registry: &mut CharRegistry,
    max_edits: usize,
    max_lines: usize,
    _allow_destructive: bool,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
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
        0,
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

/// Reconstruct the char-per-line model from actual file content on disk.
fn reconstruct_lines_from_content(content: &str) -> Vec<char> {
    content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().next().unwrap())
        .collect()
}
