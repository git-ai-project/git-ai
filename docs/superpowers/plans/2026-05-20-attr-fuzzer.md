# Attribution Fuzzer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a randomized end-to-end fuzzer that verifies git-ai line-level attribution correctness through edits, checkpoints, commits, and rewrite operations (amend, cherry-pick, rebase, squash merge).

**Architecture:** A char-based oracle where each edit step uses a unique character mapped to an attribution type. The fuzzer generates random operation sequences from a seed, executes them against a real TestRepo with shared daemon, and verifies blame output matches the expected attribution for each character. Deterministic seeds make failures reproducible.

**Tech Stack:** Rust, rand 0.10 (SmallRng + SeedableRng), existing TestRepo infrastructure, git-ai blame

---

## File Structure

```
tests/fuzzer/
├── mod.rs              — #[test] entry points, run_fuzzer() dispatcher
├── oracle.rs           — CharRegistry: char allocation + blame verification
├── operations.rs       — Operation enum, EditStrategy, execution against TestRepo
├── engine.rs           — FuzzerEngine: RNG-driven scenario orchestration
└── generators.rs       — Random parameter generation (attribution, strategy, line counts)
```

Additionally:
- Modify: `tests/integration/main.rs` — add `mod fuzzer;` declaration
- Modify: `Taskfile.yml` — add `test:fuzz` and `test:fuzz:heavy` tasks

---

### Task 1: Oracle Module — CharRegistry

**Files:**
- Create: `tests/fuzzer/oracle.rs`

- [ ] **Step 1: Create the oracle module with CharRegistry struct**

```rust
// tests/fuzzer/oracle.rs
use crate::repos::test_file::AuthorType;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Attribution {
    Ai,
    KnownHuman,
    Untracked,
}

impl Attribution {
    pub fn to_author_type(self) -> AuthorType {
        match self {
            Attribution::Ai => AuthorType::Ai,
            Attribution::KnownHuman => AuthorType::Human,
            Attribution::Untracked => AuthorType::UnattributedHuman,
        }
    }

    pub fn checkpoint_command(&self) -> &'static str {
        match self {
            Attribution::Ai => "mock_ai",
            Attribution::KnownHuman => "mock_known_human",
            Attribution::Untracked => "human",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CharEntry {
    pub ch: char,
    pub attribution: Attribution,
    pub step_order: usize,
}

pub struct CharRegistry {
    entries: Vec<CharEntry>,
    next_index: usize,
}

const CHAR_POOL: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

impl CharRegistry {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_index: 0,
        }
    }

    pub fn allocate(&mut self, attribution: Attribution) -> char {
        let ch = CHAR_POOL.chars().nth(self.next_index)
            .unwrap_or_else(|| {
                // Overflow into Unicode block
                char::from_u32(0x0391 + (self.next_index - CHAR_POOL.len()) as u32)
                    .unwrap_or('?')
            });
        let entry = CharEntry {
            ch,
            attribution,
            step_order: self.next_index,
        };
        self.entries.push(entry);
        self.next_index += 1;
        ch
    }

    pub fn lookup(&self, ch: char) -> Option<&CharEntry> {
        self.entries.iter().find(|e| e.ch == ch)
    }

    pub fn dump(&self) -> String {
        self.entries
            .iter()
            .map(|e| format!("  '{}' (step {}) -> {:?}", e.ch, e.step_order, e.attribution))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
```

- [ ] **Step 2: Add the verify_blame function**

Append to `tests/fuzzer/oracle.rs`:

```rust
use crate::repos::test_repo::TestRepo;

pub struct BlameVerificationError {
    pub line_num: usize,
    pub content: String,
    pub ch: char,
    pub expected: Attribution,
    pub actual_author: String,
    pub is_ai: bool,
}

impl CharRegistry {
    pub fn verify_blame(
        &self,
        repo: &TestRepo,
        filename: &str,
        operation_log: &[String],
    ) {
        let file_path = repo.path().join(filename);
        let blame_output = repo
            .git_ai(&["blame", file_path.to_str().unwrap()])
            .unwrap_or_else(|e| panic!("blame failed: {e}"));

        let mut errors: Vec<BlameVerificationError> = Vec::new();

        for (i, line) in blame_output.lines().filter(|l| !l.trim().is_empty()).enumerate() {
            let (author, content) = parse_blame_line(line);

            if content.trim().is_empty() {
                continue;
            }

            // The line content is a single char repeated — extract the char
            let ch = content.trim().chars().next().unwrap();

            let entry = match self.lookup(ch) {
                Some(e) => e,
                None => {
                    panic!(
                        "Line {} has char '{}' not in registry!\nBlame: {}\nRegistry:\n{}",
                        i + 1, ch, blame_output, self.dump()
                    );
                }
            };

            let is_ai = is_ai_author(&author);
            let matches = match entry.attribution {
                Attribution::Ai => is_ai,
                Attribution::KnownHuman | Attribution::Untracked => !is_ai,
            };

            if !matches {
                errors.push(BlameVerificationError {
                    line_num: i + 1,
                    content: content.clone(),
                    ch,
                    expected: entry.attribution,
                    actual_author: author.clone(),
                    is_ai,
                });
            }
        }

        if !errors.is_empty() {
            let mut msg = format!(
                "\nFUZZER BLAME VERIFICATION FAILED ({} errors)\n\n",
                errors.len()
            );
            for err in &errors {
                msg.push_str(&format!(
                    "  Line {}: char='{}' expected={:?} actual_author='{}' (is_ai={})\n",
                    err.line_num, err.ch, err.expected, err.actual_author, err.is_ai
                ));
            }
            msg.push_str("\nChar Registry:\n");
            msg.push_str(&self.dump());
            msg.push_str("\n\nOperation Log:\n");
            for (i, op) in operation_log.iter().enumerate() {
                msg.push_str(&format!("  [{}] {}\n", i, op));
            }
            msg.push_str(&format!("\nFull blame output:\n{}\n", blame_output));
            panic!("{}", msg);
        }
    }
}

fn parse_blame_line(line: &str) -> (String, String) {
    if let Some(start_paren) = line.find('(')
        && let Some(end_paren) = line.find(')')
    {
        let author_section = &line[start_paren + 1..end_paren];
        let content = line[end_paren + 1..].trim();
        let parts: Vec<&str> = author_section.split_whitespace().collect();
        let mut author_parts = Vec::new();
        for part in parts {
            if part.chars().next().unwrap_or('a').is_ascii_digit() {
                break;
            }
            author_parts.push(part);
        }
        let author = author_parts.join(" ");
        return (author, content.to_string());
    }
    ("unknown".to_string(), line.to_string())
}

const AI_AUTHOR_NAMES: &[&str] = &[
    "mock_ai", "claude", "continue-cli", "gpt", "copilot", "cursor",
    "codex", "gemini", "amp", "windsurf", "devin", "cloud-agent",
    "codex-cloud", "git-ai-cloud-agent",
];

fn is_ai_author(author: &str) -> bool {
    let name_only = if let Some(bracket) = author.find('<') {
        &author[..bracket]
    } else {
        author
    };
    let name_lower = name_only.to_lowercase();
    AI_AUTHOR_NAMES.iter().any(|&ai_name| name_lower.contains(ai_name))
}
```

- [ ] **Step 3: Verify it compiles**

Run: `task build`
Expected: Compiles (we'll wire up the module in a later task)

---

### Task 2: Generators Module

**Files:**
- Create: `tests/fuzzer/generators.rs`

- [ ] **Step 1: Create the generators module**

```rust
// tests/fuzzer/generators.rs
use rand::Rng;
use rand::rngs::SmallRng;
use crate::fuzzer::oracle::Attribution;

#[derive(Debug, Clone, Copy)]
pub enum EditStrategy {
    Append,
    Prepend,
    InsertRandom,
    ReplaceRandom,
    DeleteAndInsert,
    OverwriteAll,
}

#[derive(Debug, Clone, Copy)]
pub enum Phase {
    Linear,
    Rewrite,
}

#[derive(Debug, Clone)]
pub enum RewriteOp {
    Amend,
    CherryPick,
    Rebase,
    SquashMerge,
}

impl EditStrategy {
    pub fn gen(rng: &mut SmallRng) -> Self {
        match rng.random_range(0u8..6) {
            0 => Self::Append,
            1 => Self::Prepend,
            2 => Self::InsertRandom,
            3 => Self::ReplaceRandom,
            4 => Self::DeleteAndInsert,
            _ => Self::OverwriteAll,
        }
    }

    /// Generate a strategy that only adds/replaces (no full overwrite) for
    /// scenarios where we need to preserve some existing content
    pub fn gen_non_destructive(rng: &mut SmallRng) -> Self {
        match rng.random_range(0u8..4) {
            0 => Self::Append,
            1 => Self::Prepend,
            2 => Self::InsertRandom,
            _ => Self::ReplaceRandom,
        }
    }
}

pub fn gen_attribution(rng: &mut SmallRng) -> Attribution {
    // 50% AI, 30% KnownHuman, 20% Untracked
    let roll: u8 = rng.random_range(0..10);
    match roll {
        0..5 => Attribution::Ai,
        5..8 => Attribution::KnownHuman,
        _ => Attribution::Untracked,
    }
}

pub fn gen_line_count(rng: &mut SmallRng, max: usize) -> usize {
    rng.random_range(1..=max.max(1))
}

pub fn gen_rewrite_op(rng: &mut SmallRng) -> RewriteOp {
    match rng.random_range(0u8..4) {
        0 => RewriteOp::Amend,
        1 => RewriteOp::CherryPick,
        2 => RewriteOp::Rebase,
        _ => RewriteOp::SquashMerge,
    }
}

pub fn gen_line_content(ch: char, line_count: usize, rng: &mut SmallRng) -> Vec<String> {
    (0..line_count)
        .map(|_| {
            let repeat = rng.random_range(5..=20);
            std::iter::repeat(ch).take(repeat).collect()
        })
        .collect()
}
```

- [ ] **Step 2: Verify it compiles**

Run: `task build`
Expected: Compiles

---

### Task 3: Operations Module

**Files:**
- Create: `tests/fuzzer/operations.rs`

- [ ] **Step 1: Create the operations module with edit execution**

```rust
// tests/fuzzer/operations.rs
use std::fs;
use rand::Rng;
use rand::rngs::SmallRng;
use crate::repos::test_repo::TestRepo;
use crate::fuzzer::oracle::{Attribution, CharRegistry};
use crate::fuzzer::generators::{EditStrategy, gen_line_content};

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

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn write_to_disk(&self, repo: &TestRepo, registry: &CharRegistry, rng: &mut SmallRng) {
        let content: String = self.lines.iter().map(|&ch| {
            let repeat = rng.random_range(5..=20);
            let line: String = std::iter::repeat(ch).take(repeat).collect();
            format!("{}\n", line)
        }).collect();
        let path = repo.path().join(&self.filename);
        fs::write(&path, content).unwrap();
    }

    pub fn apply_edit(
        &mut self,
        strategy: EditStrategy,
        ch: char,
        line_count: usize,
        rng: &mut SmallRng,
    ) {
        let new_lines: Vec<char> = vec![ch; line_count];

        match strategy {
            EditStrategy::Append => {
                self.lines.extend(new_lines);
            }
            EditStrategy::Prepend => {
                self.lines.splice(0..0, new_lines);
            }
            EditStrategy::InsertRandom => {
                let pos = if self.lines.is_empty() {
                    0
                } else {
                    rng.random_range(0..=self.lines.len())
                };
                self.lines.splice(pos..pos, new_lines);
            }
            EditStrategy::ReplaceRandom => {
                if self.lines.is_empty() {
                    self.lines.extend(new_lines);
                } else {
                    let max_start = self.lines.len().saturating_sub(1);
                    let start = rng.random_range(0..=max_start);
                    let end = (start + line_count).min(self.lines.len());
                    self.lines.splice(start..end, new_lines);
                }
            }
            EditStrategy::DeleteAndInsert => {
                if self.lines.is_empty() {
                    self.lines.extend(new_lines);
                } else {
                    // Delete some random lines first
                    let delete_count = rng.random_range(1..=self.lines.len().max(1));
                    let start = rng.random_range(0..self.lines.len());
                    let end = (start + delete_count).min(self.lines.len());
                    self.lines.drain(start..end);
                    // Insert at a random position
                    let pos = if self.lines.is_empty() {
                        0
                    } else {
                        rng.random_range(0..=self.lines.len())
                    };
                    self.lines.splice(pos..pos, new_lines);
                }
            }
            EditStrategy::OverwriteAll => {
                self.lines = new_lines;
            }
        }
    }
}

pub fn execute_edit_and_checkpoint(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    attribution: Attribution,
    strategy: EditStrategy,
    line_count: usize,
    rng: &mut SmallRng,
    operation_log: &mut Vec<String>,
) -> char {
    let ch = registry.allocate(attribution);

    operation_log.push(format!(
        "EditAndCheckpoint({:?}, {} lines, {:?}) -> char '{}'",
        attribution, line_count, strategy, ch
    ));

    // For untracked attribution, we simulate the AI agent preset pre-edit checkpoint
    // (which captures existing state as "untracked") followed by writing new content
    if matches!(attribution, Attribution::Untracked) {
        // Fire pre-edit checkpoint to mark current state
        repo.git_ai(&["checkpoint", "human", &file_state.filename]).ok();
    }

    file_state.apply_edit(strategy, ch, line_count, rng);
    file_state.write_to_disk(repo, registry, rng);

    // Fire the checkpoint
    match attribution {
        Attribution::Ai => {
            repo.git_ai(&["checkpoint", "mock_ai", &file_state.filename]).unwrap();
        }
        Attribution::KnownHuman => {
            repo.git_ai(&["checkpoint", "mock_known_human", &file_state.filename]).unwrap();
        }
        Attribution::Untracked => {
            // For untracked, we already fired the human checkpoint above and wrote new content.
            // The untracked scenario is: changes appear between checkpoints with no explicit
            // AI or human checkpoint covering them. So we DON'T fire another checkpoint here.
            // The changes will be caught as "untracked" at commit time.
        }
    }

    ch
}

pub fn execute_commit(
    repo: &TestRepo,
    message: &str,
    operation_log: &mut Vec<String>,
) {
    operation_log.push(format!("Commit(\"{}\")", message));
    repo.git(&["add", "-A"]).unwrap();
    repo.commit(message).unwrap();
}

pub fn execute_amend(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut SmallRng,
    operation_log: &mut Vec<String>,
) {
    let attribution = crate::fuzzer::generators::gen_attribution(rng);
    let strategy = EditStrategy::gen_non_destructive(rng);
    let line_count = crate::fuzzer::generators::gen_line_count(rng, 3);

    let ch = execute_edit_and_checkpoint(
        repo, file_state, registry, attribution, strategy, line_count, rng, operation_log,
    );

    operation_log.push(format!("Amend (with char '{}')", ch));
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "Amended commit"]).unwrap();
}

pub fn execute_cherry_pick(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut SmallRng,
    operation_log: &mut Vec<String>,
) {
    let main_branch = repo.current_branch();

    // Create a side branch
    repo.git(&["checkout", "-b", "cherry-pick-branch"]).unwrap();

    // Make an edit on the side branch
    let attribution = crate::fuzzer::generators::gen_attribution(rng);
    let strategy = EditStrategy::gen_non_destructive(rng);
    let line_count = crate::fuzzer::generators::gen_line_count(rng, 3);
    let ch = execute_edit_and_checkpoint(
        repo, file_state, registry, attribution, strategy, line_count, rng, operation_log,
    );
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("Cherry-pick source commit").unwrap();

    let commit_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Switch back to main and cherry-pick
    repo.git(&["checkout", &main_branch]).unwrap();

    operation_log.push(format!("CherryPick(commit={}, char='{}')", &commit_sha[..8], ch));
    repo.git(&["cherry-pick", &commit_sha]).unwrap();

    // Clean up branch
    repo.git(&["branch", "-D", "cherry-pick-branch"]).unwrap();
}

pub fn execute_rebase(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut SmallRng,
    operation_log: &mut Vec<String>,
) {
    let main_branch = repo.current_branch();

    // Create a feature branch from current HEAD
    repo.git(&["checkout", "-b", "rebase-branch"]).unwrap();

    // Make an edit on the feature branch (non-conflicting: use a separate file)
    let attribution = crate::fuzzer::generators::gen_attribution(rng);
    let line_count = crate::fuzzer::generators::gen_line_count(rng, 3);
    let rebase_file = format!("rebase_{}.txt", registry.next_index());
    let ch = registry.allocate(attribution);
    let content: String = (0..line_count).map(|_| {
        let repeat = rng.random_range(5..=20);
        let line: String = std::iter::repeat(ch).take(repeat).collect();
        format!("{}\n", line)
    }).collect();
    let path = repo.path().join(&rebase_file);
    fs::write(&path, &content).unwrap();

    match attribution {
        Attribution::Ai => {
            repo.git_ai(&["checkpoint", "mock_ai", &rebase_file]).unwrap();
        }
        Attribution::KnownHuman => {
            repo.git_ai(&["checkpoint", "mock_known_human", &rebase_file]).unwrap();
        }
        Attribution::Untracked => {
            repo.git_ai(&["checkpoint", "human", &rebase_file]).ok();
        }
    }
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("Rebase feature commit").unwrap();

    operation_log.push(format!(
        "Rebase(file={}, char='{}', {:?})",
        rebase_file, ch, attribution
    ));

    // Go back to main and make a non-conflicting commit
    repo.git(&["checkout", &main_branch]).unwrap();
    let dummy_file = format!("main_advance_{}.txt", registry.next_index());
    fs::write(repo.path().join(&dummy_file), "main advance\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.commit("Main advance for rebase").unwrap();

    // Rebase feature branch onto main
    repo.git(&["checkout", "rebase-branch"]).unwrap();
    repo.git(&["rebase", &main_branch]).unwrap();

    // Merge back to main (fast-forward)
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "rebase-branch"]).unwrap();

    // Clean up
    repo.git(&["branch", "-d", "rebase-branch"]).unwrap();
}

pub fn execute_squash_merge(
    repo: &TestRepo,
    file_state: &mut FileState,
    registry: &mut CharRegistry,
    rng: &mut SmallRng,
    operation_log: &mut Vec<String>,
) {
    let main_branch = repo.current_branch();

    // Create a feature branch
    repo.git(&["checkout", "-b", "squash-branch"]).unwrap();

    // Make 2-3 commits on the feature branch using a separate file
    let commit_count = rng.random_range(2..=3);
    let squash_file = format!("squash_{}.txt", registry.next_index());
    let mut squash_content = String::new();

    for i in 0..commit_count {
        let attribution = crate::fuzzer::generators::gen_attribution(rng);
        let line_count = crate::fuzzer::generators::gen_line_count(rng, 3);
        let ch = registry.allocate(attribution);

        for _ in 0..line_count {
            let repeat = rng.random_range(5..=20);
            let line: String = std::iter::repeat(ch).take(repeat).collect();
            squash_content.push_str(&line);
            squash_content.push('\n');
        }
        fs::write(repo.path().join(&squash_file), &squash_content).unwrap();

        match attribution {
            Attribution::Ai => {
                repo.git_ai(&["checkpoint", "mock_ai", &squash_file]).unwrap();
            }
            Attribution::KnownHuman => {
                repo.git_ai(&["checkpoint", "mock_known_human", &squash_file]).unwrap();
            }
            Attribution::Untracked => {
                repo.git_ai(&["checkpoint", "human", &squash_file]).ok();
            }
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.commit(&format!("Squash commit {}", i + 1)).unwrap();
    }

    operation_log.push(format!(
        "SquashMerge(file={}, {} commits)",
        squash_file, commit_count
    ));

    // Switch back and squash merge
    repo.git(&["checkout", &main_branch]).unwrap();
    repo.git(&["merge", "--squash", "squash-branch"]).unwrap();
    repo.commit("Squashed feature").unwrap();

    // Clean up
    repo.git(&["branch", "-D", "squash-branch"]).unwrap();
}
```

- [ ] **Step 2: Add `next_index` getter to CharRegistry**

In `oracle.rs`, add to the `impl CharRegistry` block:

```rust
    pub fn next_index(&self) -> usize {
        self.next_index
    }
```

- [ ] **Step 3: Verify it compiles**

Run: `task build`
Expected: Compiles

---

### Task 4: Engine Module

**Files:**
- Create: `tests/fuzzer/engine.rs`

- [ ] **Step 1: Create the engine module**

```rust
// tests/fuzzer/engine.rs
use rand::SeedableRng;
use rand::Rng;
use rand::rngs::SmallRng;
use crate::repos::test_repo::TestRepo;
use crate::fuzzer::oracle::CharRegistry;
use crate::fuzzer::generators::{self, EditStrategy};
use crate::fuzzer::operations::{self, FileState};

pub struct FuzzerConfig {
    pub seed: u64,
    pub total_ops: usize,
    pub linear_ops_ratio: f32,
    pub max_lines_per_edit: usize,
}

impl FuzzerConfig {
    pub fn standard(seed: u64, total_ops: usize) -> Self {
        Self {
            seed,
            total_ops,
            linear_ops_ratio: 0.6,
            max_lines_per_edit: 8,
        }
    }

    pub fn rewrite_heavy(seed: u64, total_ops: usize) -> Self {
        Self {
            seed,
            total_ops,
            linear_ops_ratio: 0.3,
            max_lines_per_edit: 5,
        }
    }

    pub fn checkpoint_heavy(seed: u64, total_ops: usize) -> Self {
        Self {
            seed,
            total_ops,
            linear_ops_ratio: 0.9,
            max_lines_per_edit: 10,
        }
    }
}

pub fn run_fuzzer(config: FuzzerConfig) {
    let mut rng = SmallRng::seed_from_u64(config.seed);
    let repo = TestRepo::new();
    let mut registry = CharRegistry::new();
    let mut operation_log: Vec<String> = Vec::new();
    let mut file_state = FileState::new("fuzz_target.txt");

    eprintln!("[fuzzer] seed={} ops={}", config.seed, config.total_ops);

    // Phase 1: Initial setup — create file with first edit and commit
    let initial_attribution = generators::gen_attribution(&mut rng);
    let initial_lines = generators::gen_line_count(&mut rng, config.max_lines_per_edit);
    let initial_strategy = EditStrategy::Append; // Always append for first edit

    operations::execute_edit_and_checkpoint(
        &repo,
        &mut file_state,
        &mut registry,
        initial_attribution,
        initial_strategy,
        initial_lines,
        &mut rng,
        &mut operation_log,
    );
    operations::execute_commit(&repo, "Initial commit", &mut operation_log);
    registry.verify_blame(&repo, "fuzz_target.txt", &operation_log);

    // Phase 2 & 3: Interleaved linear edits and rewrites
    let linear_op_count = (config.total_ops as f32 * config.linear_ops_ratio) as usize;
    let rewrite_op_count = config.total_ops - linear_op_count;

    let mut edits_since_last_commit = 0;
    let commit_frequency = rng.random_range(1..=3);

    // Phase 2: Linear edits
    for i in 0..linear_op_count {
        let attribution = generators::gen_attribution(&mut rng);
        let strategy = if file_state.line_count() == 0 {
            EditStrategy::Append
        } else {
            EditStrategy::gen(&mut rng)
        };
        let line_count = generators::gen_line_count(&mut rng, config.max_lines_per_edit);

        operations::execute_edit_and_checkpoint(
            &repo,
            &mut file_state,
            &mut registry,
            attribution,
            strategy,
            line_count,
            &mut rng,
            &mut operation_log,
        );

        edits_since_last_commit += 1;

        if edits_since_last_commit >= commit_frequency || i == linear_op_count - 1 {
            operations::execute_commit(
                &repo,
                &format!("Linear commit {}", i),
                &mut operation_log,
            );
            registry.verify_blame(&repo, "fuzz_target.txt", &operation_log);
            edits_since_last_commit = 0;
        }
    }

    // Phase 3: Rewrite operations
    for i in 0..rewrite_op_count {
        let op = generators::gen_rewrite_op(&mut rng);
        match op {
            generators::RewriteOp::Amend => {
                // Make sure there's at least one commit to amend
                if file_state.line_count() > 0 {
                    operations::execute_amend(
                        &repo,
                        &mut file_state,
                        &mut registry,
                        &mut rng,
                        &mut operation_log,
                    );
                    registry.verify_blame(&repo, "fuzz_target.txt", &operation_log);
                }
            }
            generators::RewriteOp::CherryPick => {
                operations::execute_cherry_pick(
                    &repo,
                    &mut file_state,
                    &mut registry,
                    &mut rng,
                    &mut operation_log,
                );
                registry.verify_blame(&repo, "fuzz_target.txt", &operation_log);
            }
            generators::RewriteOp::Rebase => {
                operations::execute_rebase(
                    &repo,
                    &mut file_state,
                    &mut registry,
                    &mut rng,
                    &mut operation_log,
                );
                // Verify the main target file (rebase uses separate files to avoid conflicts)
                registry.verify_blame(&repo, "fuzz_target.txt", &operation_log);
            }
            generators::RewriteOp::SquashMerge => {
                operations::execute_squash_merge(
                    &repo,
                    &mut file_state,
                    &mut registry,
                    &mut rng,
                    &mut operation_log,
                );
                // Verify the main target file
                registry.verify_blame(&repo, "fuzz_target.txt", &operation_log);
            }
        }

        eprintln!(
            "[fuzzer] rewrite op {}/{} complete (seed={})",
            i + 1, rewrite_op_count, config.seed
        );
    }

    eprintln!(
        "[fuzzer] PASSED seed={} ({} ops, {} chars allocated)",
        config.seed, config.total_ops, registry.next_index()
    );
}
```

- [ ] **Step 2: Verify it compiles**

Run: `task build`
Expected: Compiles

---

### Task 5: Module Entry Point and Test Functions

**Files:**
- Create: `tests/fuzzer/mod.rs`
- Modify: `tests/integration/main.rs` — add `mod fuzzer;`

- [ ] **Step 1: Create the fuzzer mod.rs with test entry points**

```rust
// tests/fuzzer/mod.rs
mod oracle;
mod generators;
mod operations;
mod engine;

use engine::{FuzzerConfig, run_fuzzer};

// Fixed seed tests — deterministic and reproducible
#[test]
fn fuzz_seed_0() { run_fuzzer(FuzzerConfig::standard(0, 50)); }

#[test]
fn fuzz_seed_1() { run_fuzzer(FuzzerConfig::standard(1, 50)); }

#[test]
fn fuzz_seed_2() { run_fuzzer(FuzzerConfig::standard(2, 50)); }

#[test]
fn fuzz_seed_3() { run_fuzzer(FuzzerConfig::standard(3, 50)); }

#[test]
fn fuzz_seed_4() { run_fuzzer(FuzzerConfig::standard(4, 50)); }

#[test]
fn fuzz_seed_5() { run_fuzzer(FuzzerConfig::standard(5, 50)); }

#[test]
fn fuzz_seed_6() { run_fuzzer(FuzzerConfig::standard(6, 50)); }

#[test]
fn fuzz_seed_7() { run_fuzzer(FuzzerConfig::standard(7, 50)); }

#[test]
fn fuzz_seed_8() { run_fuzzer(FuzzerConfig::standard(8, 50)); }

#[test]
fn fuzz_seed_9() { run_fuzzer(FuzzerConfig::standard(9, 50)); }

// Random seed test — prints seed on failure for reproduction
#[test]
fn fuzz_random_seed() {
    let seed: u64 = rand::random();
    eprintln!("FUZZER RANDOM SEED: {seed} — use this to reproduce failures");
    run_fuzzer(FuzzerConfig::standard(seed, 100));
}

// Rewrite-heavy variant — focuses on amend/cherry-pick/rebase/squash
#[test]
fn fuzz_heavy_rewrite_seed_42() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(42, 30));
}

#[test]
fn fuzz_heavy_rewrite_seed_99() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(99, 30));
}

#[test]
fn fuzz_heavy_rewrite_seed_777() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(777, 30));
}

// Checkpoint-heavy variant — rapid fire checkpoints to stress daemon
#[test]
fn fuzz_rapid_checkpoints_seed_0() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(0, 80));
}

#[test]
fn fuzz_rapid_checkpoints_seed_1() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(1, 80));
}

#[test]
fn fuzz_rapid_checkpoints_seed_2() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(2, 80));
}
```

- [ ] **Step 2: Add mod fuzzer to integration main.rs**

In `tests/integration/main.rs`, add at the end of the module declarations:

```rust
mod fuzzer;
```

Note: The fuzzer directory must be placed at `tests/integration/fuzzer/` since it's a submodule of the integration test binary. Adjust the file paths in Tasks 1-4 accordingly — all files go under `tests/integration/fuzzer/`.

- [ ] **Step 3: Verify compilation**

Run: `task build`
Expected: Compiles successfully

- [ ] **Step 4: Run a single fuzzer test to verify basic operation**

Run: `task test TEST_FILTER=fuzz_seed_0 NO_CAPTURE=true`
Expected: Test passes (or fails with an attribution bug — which is the point!)

- [ ] **Step 5: Commit**

```bash
git add tests/integration/fuzzer/ tests/integration/main.rs
git commit -m "feat: add attribution fuzzer for e2e randomized testing"
```

---

### Task 6: Taskfile Integration

**Files:**
- Modify: `Taskfile.yml`

- [ ] **Step 1: Add fuzzer tasks to Taskfile.yml**

Add after the existing `test:wrapper-daemon` task:

```yaml
  test:fuzz:
    desc: Run the attribution fuzzer (fixed seeds)
    cmds:
      - task: test:base
        vars:
          GIT_AI_TEST_GIT_MODE: daemon
          TEST_FILTER: fuzz_seed

  test:fuzz:all:
    desc: Run all fuzzer tests including random seed and heavy variants
    cmds:
      - task: test:base
        vars:
          GIT_AI_TEST_GIT_MODE: daemon
          TEST_FILTER: fuzz_

  test:fuzz:heavy:
    desc: Run fuzzer with verbose output
    cmds:
      - task: test:base
        vars:
          GIT_AI_TEST_GIT_MODE: daemon
          TEST_FILTER: fuzz_
          NO_CAPTURE: "true"
```

- [ ] **Step 2: Verify tasks work**

Run: `task test:fuzz`
Expected: Runs all `fuzz_seed_*` tests

- [ ] **Step 3: Commit**

```bash
git add Taskfile.yml
git commit -m "chore: add task test:fuzz commands for attribution fuzzer"
```

---

### Task 7: Fix Compilation Issues and Iterate

This task handles any compilation or runtime issues discovered during Tasks 1-6. The plan above uses the exact patterns from the codebase (`repo.git_ai(&[...])`, `repo.git(&[...])`, `repo.commit(...)`, `rand::random_range(...)`) but minor adjustments may be needed.

**Files:**
- Modify: Any file in `tests/integration/fuzzer/`

- [ ] **Step 1: Fix any import path issues**

Key imports needed across fuzzer modules:
```rust
// In oracle.rs
use crate::repos::test_repo::TestRepo;

// In operations.rs  
use crate::repos::test_repo::TestRepo;
use crate::fuzzer::oracle::{Attribution, CharRegistry};
use crate::fuzzer::generators::EditStrategy;

// In engine.rs
use crate::repos::test_repo::TestRepo;
use crate::fuzzer::oracle::CharRegistry;
use crate::fuzzer::generators;
use crate::fuzzer::operations::{self, FileState};
```

- [ ] **Step 2: Run full fuzzer suite**

Run: `task test:fuzz:all NO_CAPTURE=true`
Expected: All tests pass OR failures indicate real attribution bugs

- [ ] **Step 3: Fix any runtime issues**

Common issues to watch for:
- Branch name conflicts if tests run too fast (add unique suffixes from registry index)
- Empty file edge cases in blame parsing
- Daemon sync timing (blame should auto-sync, but verify)

- [ ] **Step 4: Final commit with fixes**

```bash
git add tests/integration/fuzzer/
git commit -m "fix: resolve fuzzer compilation and runtime issues"
```

---

## Key Implementation Notes

1. **File placement**: All fuzzer files go in `tests/integration/fuzzer/` (not `tests/fuzzer/`) because they're submodules of the `integration` test binary declared in `tests/integration/main.rs`.

2. **RNG**: Use `rand::rngs::SmallRng` with `SeedableRng::seed_from_u64(seed)` for deterministic seeding. The project already has `rand = "0.10"`.

3. **No manual daemon sync**: The `repo.git_ai(&["blame", ...])` call in TestRepo automatically triggers `sync_daemon_force()` before executing. This is the only point where sync happens.

4. **Branch naming**: Rewrite operations create temporary branches. Use `registry.next_index()` in branch names to avoid collisions between operations within the same test.

5. **Separate files for rebase/squash**: These operations use separate files (not `fuzz_target.txt`) to avoid merge conflicts that would require manual resolution. The main file is still verified after each operation.

6. **The `write_to_disk` method uses a fresh rng for line length**: Each time the file is written, line lengths are randomly chosen (5-20 chars). This means the same logical state can have different physical content across writes — which is fine because verification only looks at the first char of each line.
