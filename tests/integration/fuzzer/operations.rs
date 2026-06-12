use rand::{Rng, RngExt};

use crate::repos::test_repo::TestRepo;

use super::model::{AttrRegistry, FileModel, LineAttribution};

pub struct CharAllocator {
    next: u32,
}

impl CharAllocator {
    pub fn new() -> Self {
        Self { next: 0x4E00 }
    }

    pub fn alloc(&mut self) -> char {
        let ch = char::from_u32(self.next).unwrap_or('?');
        self.next += 1;
        ch
    }
}

/// Edit the file: insert, append, replace, or delete lines.
/// Returns the chars that were written (for checkpointing).
/// All new chars start as Untracked in the registry until checkpointed.
pub fn random_edit(
    model: &mut FileModel,
    registry: &mut AttrRegistry,
    repo: &TestRepo,
    alloc: &mut CharAllocator,
    rng: &mut impl Rng,
    max_lines: usize,
) -> Vec<char> {
    let num_lines = rng.random_range(1..=max_lines);
    let new_chars: Vec<char> = (0..num_lines).map(|_| alloc.alloc()).collect();

    // Register all new chars as Untracked initially
    for &ch in &new_chars {
        registry.register(ch, LineAttribution::Untracked);
    }

    if model.lines.is_empty() {
        for &ch in &new_chars {
            model.lines.push(ch);
            model.resolved_attrs.push(LineAttribution::Untracked);
        }
    } else {
        let strategy = rng.random_range(0..4);
        match strategy {
            0 => {
                let pos = rng.random_range(0..=model.lines.len());
                for (j, &ch) in new_chars.iter().enumerate() {
                    model.lines.insert(pos + j, ch);
                    model
                        .resolved_attrs
                        .insert(pos + j, LineAttribution::Untracked);
                }
            }
            1 => {
                let start = rng.random_range(0..model.lines.len());
                let end = (start + num_lines).min(model.lines.len());
                let replace_count = end - start;
                for (j, &ch) in new_chars.iter().take(replace_count).enumerate() {
                    model.lines[start + j] = ch;
                    model.resolved_attrs[start + j] = LineAttribution::Untracked;
                }
                for &ch in new_chars.iter().skip(replace_count) {
                    model.lines.insert(end, ch);
                    model.resolved_attrs.insert(end, LineAttribution::Untracked);
                }
            }
            2 => {
                for &ch in &new_chars {
                    model.lines.push(ch);
                    model.resolved_attrs.push(LineAttribution::Untracked);
                }
            }
            3 => {
                if model.lines.len() > 1 {
                    let del_count = rng.random_range(1..model.lines.len().min(4));
                    let del_start = rng.random_range(0..model.lines.len() - del_count + 1);
                    model.lines.drain(del_start..del_start + del_count);
                    model.resolved_attrs.drain(del_start..del_start + del_count);
                }
                let pos = if model.lines.is_empty() {
                    0
                } else {
                    rng.random_range(0..=model.lines.len())
                };
                for (j, &ch) in new_chars.iter().enumerate() {
                    model.lines.insert(pos + j, ch);
                    model
                        .resolved_attrs
                        .insert(pos + j, LineAttribution::Untracked);
                }
            }
            _ => unreachable!(),
        }
    }

    model.write_to_disk(repo);
    new_chars
}

/// Checkpoint as AI — marks the written chars as Ai in registry and model.
pub fn checkpoint_ai(
    model: &mut FileModel,
    registry: &mut AttrRegistry,
    repo: &TestRepo,
    written_chars: &[char],
    op_log: &mut Vec<String>,
) {
    repo.git_ai(&["checkpoint", "mock_ai", &model.filename])
        .unwrap_or_else(|e| panic!("checkpoint mock_ai failed: {}", e));

    for &ch in written_chars {
        registry.register(ch, LineAttribution::Ai);
    }
    for (i, &ch) in model.lines.iter().enumerate() {
        if written_chars.contains(&ch) {
            model.resolved_attrs[i] = LineAttribution::Ai;
        }
    }
    op_log.push(format!("checkpoint_ai({})", model.filename));
}

/// Checkpoint as known human — marks the written chars as KnownHuman.
pub fn checkpoint_human(
    model: &mut FileModel,
    registry: &mut AttrRegistry,
    repo: &TestRepo,
    written_chars: &[char],
    op_log: &mut Vec<String>,
) {
    repo.git_ai(&["checkpoint", "mock_known_human", &model.filename])
        .unwrap_or_else(|e| panic!("checkpoint mock_known_human failed: {}", e));

    for &ch in written_chars {
        registry.register(ch, LineAttribution::KnownHuman);
    }
    for (i, &ch) in model.lines.iter().enumerate() {
        if written_chars.contains(&ch) {
            model.resolved_attrs[i] = LineAttribution::KnownHuman;
        }
    }
    op_log.push(format!("checkpoint_human({})", model.filename));
}

/// Checkpoint as untracked (legacy "human" checkpoint).
pub fn checkpoint_untracked(model: &FileModel, repo: &TestRepo, op_log: &mut Vec<String>) {
    repo.git_ai(&["checkpoint", "human", &model.filename])
        .unwrap_or_else(|e| panic!("checkpoint human failed: {}", e));
    op_log.push(format!("checkpoint_untracked({})", model.filename));
}

/// Commit: stage all and commit. Then reconcile and assert.
pub fn commit(
    model: &mut FileModel,
    repo: &TestRepo,
    op_log: &mut Vec<String>,
    seed: u64,
    msg: &str,
) {
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", msg, "--allow-empty"])
        .unwrap_or_else(|e| panic!("commit '{}' failed: {}", msg, e));

    op_log.push(format!("commit(\"{}\")", msg));
    model.reconcile(repo);
    model.assert_blame(repo, op_log, seed);
}

/// Amend the last commit. Then reconcile and assert.
pub fn amend(
    model: &mut FileModel,
    registry: &AttrRegistry,
    repo: &TestRepo,
    op_log: &mut Vec<String>,
    seed: u64,
) {
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "--amend", "--no-edit"])
        .unwrap_or_else(|e| panic!("amend failed: {}", e));

    op_log.push("amend".to_string());
    model.sync_from_disk(repo, registry);
    model.reconcile(repo);
    model.assert_blame(repo, op_log, seed);
}

/// Rebase: creates a side branch with commits, then rebases onto main.
pub fn rebase(
    model: &mut FileModel,
    registry: &mut AttrRegistry,
    repo: &TestRepo,
    alloc: &mut CharAllocator,
    rng: &mut impl Rng,
    op_log: &mut Vec<String>,
    seed: u64,
) {
    let main_branch = repo
        .git(&["branch", "--show-current"])
        .unwrap()
        .trim()
        .to_string();

    // Create a commit on main first (so rebase has something to replay onto)
    let chars = random_edit(model, registry, repo, alloc, rng, 2);
    checkpoint_ai(model, registry, repo, &chars, op_log);
    commit(model, repo, op_log, seed, "rebase: main advance");

    // Create side branch from parent
    let parent = repo
        .git(&["rev-parse", "HEAD~1"])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["checkout", "-b", "rebase-side", &parent])
        .unwrap();

    // Sync model to side branch state (parent's file content)
    model.sync_from_disk(repo, registry);

    // Make a commit on the side branch
    let side_chars = random_edit(model, registry, repo, alloc, rng, 2);
    checkpoint_ai(model, registry, repo, &side_chars, op_log);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "rebase: side commit"]).unwrap();
    op_log.push("commit(\"rebase: side commit\")".to_string());

    // Rebase side onto main
    let result = repo.git(&["rebase", &main_branch]);
    if result.is_err() {
        let _ = repo.git(&["rebase", "--abort"]);
        repo.git(&["checkout", &main_branch]).unwrap();
        let _ = repo.git(&["branch", "-D", "rebase-side"]);
        model.sync_from_disk(repo, registry);
        model.reconcile(repo);
        model.assert_blame(repo, op_log, seed);
        op_log.push("rebase(aborted due to conflict)".to_string());
        return;
    }

    // Merge side back to main (fast-forward)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "rebase-side"]).unwrap();
    let _ = repo.git(&["branch", "-D", "rebase-side"]);

    op_log.push("rebase(success)".to_string());
    model.sync_from_disk(repo, registry);
    model.reconcile(repo);
    model.assert_blame(repo, op_log, seed);
}

/// Cherry-pick: creates a commit on a side branch, then cherry-picks it onto main.
pub fn cherry_pick(
    model: &mut FileModel,
    registry: &mut AttrRegistry,
    repo: &TestRepo,
    alloc: &mut CharAllocator,
    rng: &mut impl Rng,
    op_log: &mut Vec<String>,
    seed: u64,
) {
    let main_branch = repo
        .git(&["branch", "--show-current"])
        .unwrap()
        .trim()
        .to_string();

    // Create side branch from current HEAD
    repo.git(&["checkout", "-b", "cherry-side"]).unwrap();

    // Make a commit on side
    let chars = random_edit(model, registry, repo, alloc, rng, 2);
    checkpoint_ai(model, registry, repo, &chars, op_log);
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "-m", "cherry-pick: side commit"])
        .unwrap();
    let side_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Go back to main
    repo.git(&["checkout", &main_branch]).unwrap();
    model.sync_from_disk(repo, registry);

    // Cherry-pick the side commit
    let result = repo.git(&["cherry-pick", &side_sha]);
    let _ = repo.git(&["branch", "-D", "cherry-side"]);

    if result.is_err() {
        let _ = repo.git(&["cherry-pick", "--abort"]);
        op_log.push("cherry_pick(aborted due to conflict)".to_string());
        model.sync_from_disk(repo, registry);
        model.reconcile(repo);
        model.assert_blame(repo, op_log, seed);
        return;
    }

    op_log.push("cherry_pick(success)".to_string());
    model.sync_from_disk(repo, registry);
    model.reconcile(repo);
    model.assert_blame(repo, op_log, seed);
}
