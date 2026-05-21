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
                    // Nothing to replace, just append
                    for _ in 0..line_count {
                        self.lines.push(ch);
                    }
                } else {
                    // Replace up to line_count lines starting at a random position
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
                    // Delete some lines, then insert new ones at the same position
                    let max_start = self.lines.len().saturating_sub(1);
                    let start = rng.random_range(0..=max_start);
                    let delete_count = rng.random_range(1..=self.lines.len() - start).min(3);
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

    /// Write the file to disk. Each line's content is deterministically derived from
    /// its character (so the same char always produces the same line content regardless
    /// of when it's written). This is critical for git blame to correctly identify which
    /// lines were actually modified between commits.
    pub fn write_to_disk(&self, repo: &TestRepo) {
        let path = repo.path().join(&self.filename);
        let mut content = String::new();
        for &ch in &self.lines {
            // Deterministic repeat count based on the char value itself
            let repeat_count = (ch as usize % 16) + 5; // 5..=20
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

/// Parameters for a single edit operation.
pub struct EditParams {
    pub attribution: Attribution,
    pub strategy: EditStrategy,
    pub line_count: usize,
}

/// Execute an edit with proper checkpoint calls and return the allocated character.
#[allow(clippy::too_many_arguments)]
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
        Attribution::Untracked => {
            // Untracked flow: write the edit, then fire a "human" checkpoint
            // AFTER to establish the new baseline. This ensures the system knows
            // the file state changed but doesn't attribute it to AI. The "human"
            // checkpoint here acts as a fence that separates this edit from any
            // prior AI session's attribution window.
            file_state.apply_edit(*strategy, ch, *line_count, rng);
            file_state.write_to_disk(repo);
            repo.git_ai(&["checkpoint", "human", &filename]).ok();
        }
        Attribution::Ai => {
            // AI flow: pre-edit "human" checkpoint to snapshot current state,
            // then write the edit, then post-edit "mock_ai" checkpoint.
            repo.git_ai(&["checkpoint", "human", &filename]).ok();
            file_state.apply_edit(*strategy, ch, *line_count, rng);
            file_state.write_to_disk(repo);
            repo.git_ai(&["checkpoint", "mock_ai", &filename]).unwrap();
        }
        Attribution::KnownHuman => {
            // Known human: write the edit, then post-edit checkpoint.
            file_state.apply_edit(*strategy, ch, *line_count, rng);
            file_state.write_to_disk(repo);
            repo.git_ai(&["checkpoint", "mock_known_human", &filename])
                .unwrap();
        }
    }

    ch
}

/// Execute a commit (stages all and commits).
pub fn execute_commit(repo: &TestRepo, message: &str, operation_log: &mut Vec<String>) {
    operation_log.push(format!("commit: {}", message));
    repo.git(&["add", "-A"]).unwrap();
    repo.commit(message).unwrap();
}

/// Execute an amend operation: edit the file, then amend the last commit.
pub fn execute_amend(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::random_non_destructive(rng),
        line_count: gen_line_count(rng, 3),
    };

    operation_log.push("amend: starting".to_string());

    execute_edit_and_checkpoint(repo, file_state, registry, &params, rng, operation_log);

    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "amended commit"])
        .unwrap();

    operation_log.push("amend: completed".to_string());
}

/// Execute a fast-forward merge: create a branch with a SEPARATE file,
/// edit+commit there, switch back, merge (fast-forward).
/// This tests attribution preservation across branch operations.
pub fn execute_cherry_pick(
    repo: &TestRepo,
    _file_state: &mut FileState,
    registry: &mut CharRegistry,
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

    // Create a separate file state for the merge file
    let mut merge_file_state = FileState::new(&merge_filename);

    // Create and switch to feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Edit on the branch (in a separate file to avoid conflicts)
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, 3),
    };

    execute_edit_and_checkpoint(
        repo,
        &mut merge_file_state,
        registry,
        &params,
        rng,
        operation_log,
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.commit("feature branch commit").unwrap();

    // Switch back to main branch and fast-forward merge
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", &branch_name]).unwrap();

    // Cleanup branch
    repo.git(&["branch", "-d", &branch_name]).unwrap();

    operation_log.push(format!("ff-merge: completed file={}", merge_filename));
}

/// Execute a rebase using a SEPARATE file to avoid conflicts.
pub fn execute_rebase(
    repo: &TestRepo,
    _file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let branch_name = format!("rebase-{}", idx);
    let rebase_filename = format!("rebase_{}.txt", idx);
    let dummy_filename = format!("dummy_{}.txt", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!(
        "rebase: start branch={} file={}",
        branch_name, rebase_filename
    ));

    // Create a separate file state for the rebase file
    let mut rebase_file_state = FileState::new(&rebase_filename);

    // Create feature branch from current HEAD
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make an edit on the feature branch (in a separate file)
    let params = EditParams {
        attribution: gen_attribution(rng),
        strategy: EditStrategy::Append,
        line_count: gen_line_count(rng, 3),
    };

    execute_edit_and_checkpoint(
        repo,
        &mut rebase_file_state,
        registry,
        &params,
        rng,
        operation_log,
    );

    repo.git(&["add", "-A"]).unwrap();
    repo.commit("rebase feature commit").unwrap();

    // Switch back to main and advance it with a dummy file
    repo.git(&["checkout", &main_branch]).unwrap();

    let dummy_path = repo.path().join(&dummy_filename);
    fs::write(&dummy_path, "dummy content for rebase advance\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("advance main for rebase").unwrap();

    // Rebase feature branch onto main
    repo.git(&["checkout", &branch_name]).unwrap();
    repo.git(&["rebase", &main_branch]).unwrap();

    // Merge feature back to main (fast-forward)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", &branch_name]).unwrap();

    // Cleanup
    repo.git(&["branch", "-d", &branch_name]).unwrap();

    operation_log.push(format!("rebase: completed file={}", rebase_filename));
}

/// Execute a squash merge using a SEPARATE file.
pub fn execute_squash_merge(
    repo: &TestRepo,
    _file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut impl Rng,
    operation_log: &mut Vec<String>,
) {
    let idx = registry.next_index();
    let branch_name = format!("squash-{}", idx);
    let squash_filename = format!("squash_{}.txt", idx);
    let main_branch = repo.current_branch();

    operation_log.push(format!(
        "squash-merge: start branch={} file={}",
        branch_name, squash_filename
    ));

    let mut squash_file_state = FileState::new(&squash_filename);

    // Create feature branch
    repo.git(&["checkout", "-b", &branch_name]).unwrap();

    // Make 2-3 commits on the feature branch
    let commit_count = rng.random_range(2..=3u32);
    for i in 0..commit_count {
        let params = EditParams {
            attribution: gen_attribution(rng),
            strategy: EditStrategy::Append,
            line_count: gen_line_count(rng, 2),
        };

        execute_edit_and_checkpoint(
            repo,
            &mut squash_file_state,
            registry,
            &params,
            rng,
            operation_log,
        );

        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("squash branch commit {}", i + 1))
            .unwrap();
    }

    // Switch back to main
    repo.git(&["checkout", &main_branch]).unwrap();

    // Squash merge
    repo.git(&["merge", "--squash", &branch_name]).unwrap();
    repo.git(&["commit", "-m", "squash merged"]).unwrap();

    // Cleanup
    repo.git(&["branch", "-D", &branch_name]).unwrap();

    operation_log.push(format!("squash-merge: completed file={}", squash_filename));
}
