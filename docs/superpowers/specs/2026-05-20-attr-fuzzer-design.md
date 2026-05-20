# Attribution Fuzzer Design Spec

## Overview

A property-based end-to-end fuzzer that verifies git-ai tracks line-level attribution correctly through all phases of the workflow: file edits, checkpoints, commits, amends, cherry-picks, rebases, and squash merges.

## Core Insight: Char-Based Oracle

Each edit step uses a unique single character. A registry maps each character to its attribution type (AI, KnownHuman, Untracked) and the order it was written. At assertion time, the fuzzer reads blame output, sees what char is on each line, and looks up the expected attribution — no complex state tracking needed.

**Why this works:** The last writer always wins. Since each step uses a unique char, the char present on a line unambiguously identifies which step wrote it last, and therefore what attribution it should have. This holds through rewrite operations (rebase, cherry-pick) because the content doesn't change — only the commit graph topology does.

## Location

```
tests/fuzzer/
├── mod.rs              — #[test] entry points with fixed + random seeds
├── engine.rs           — FuzzerEngine orchestration
├── operations.rs       — Operation enum + execution logic
├── oracle.rs           — CharRegistry + blame verification
└── generators.rs       — Random operation/content generation
```

Integrated into the existing test crate alongside `tests/integration/`.

## Components

### CharRegistry (oracle.rs)

```rust
struct CharEntry {
    ch: char,
    attribution: Attribution,
    step_order: usize,
}

enum Attribution { Ai, KnownHuman, Untracked }

struct CharRegistry {
    entries: Vec<CharEntry>,
    next_index: usize,
}
```

- Allocates chars sequentially: A-Z, a-z, then Unicode (Greek, Cyrillic, etc.)
- Each char is permanently bound to one attribution type
- `verify_blame(blame_output, file_lines)` checks each line's char against registry

### Operations (operations.rs)

```rust
enum Operation {
    EditAndCheckpoint { attribution: Attribution, line_count: usize, strategy: EditStrategy },
    Commit,
    Amend,
    CherryPick,
    Rebase,
    SquashMerge,
}

enum EditStrategy {
    Append,
    Prepend,
    InsertRandom,
    ReplaceRandom,
    DeleteAndInsert,
    OverwriteAll,
}
```

Each operation executes against a TestRepo using raw `fs::write` + explicit `git-ai checkpoint` calls (not the TestFile helpers).

### FuzzerEngine (engine.rs)

Orchestrates scenarios in phases:

1. **Initial Setup** — Create file, first edit + checkpoint + commit, assert
2. **Linear Edits** (N iterations) — Random edits + checkpoints, periodic commits, assert after each commit
3. **Rewrite Operations** (M iterations) — Random rewrite op with new edits, assert after each

Maintains:
- `file_lines: Vec<char>` — ground truth of current file content (one char identifies each line)
- `char_registry: CharRegistry` — maps chars to attributions
- `operation_log: Vec<String>` — human-readable log for failure diagnostics
- `rng: StdRng` — seeded RNG for reproducibility

### Generators (generators.rs)

- `gen_edit_strategy(rng)` — random EditStrategy
- `gen_attribution(rng)` — random Attribution with weighted distribution (50% AI, 30% Human, 20% Untracked)
- `gen_line_count(rng, max)` — random line count 1..max
- `gen_operation(rng, phase)` — random operation appropriate for current phase
- `gen_file_content(char, count)` — generates N lines each filled with the given char repeated 5-20 times

### Assertion / Verification

After each commit or rewrite:
1. Call `repo.git_ai(&["blame", "random.txt"])` — triggers daemon sync automatically
2. Parse blame output line-by-line
3. For each line: extract content char, look up in registry, compare attribution type against blame author
4. On mismatch: print seed, full operation log, expected vs actual, registry dump

### Daemon Stress Testing

- All tests use `TestRepo::new()` (shared daemon pool)
- NO manual `sync_daemon()` calls between edits/checkpoints — only blame triggers sync
- Multiple fuzzer tests run in parallel via `cargo test` threading
- The rapid-fire checkpoint tests specifically hammer the daemon with many checkpoints before a single commit

## Test Entry Points (mod.rs)

```rust
#[test] fn fuzz_seed_0() { run_fuzzer(0, 50); }
#[test] fn fuzz_seed_1() { run_fuzzer(1, 50); }
#[test] fn fuzz_seed_2() { run_fuzzer(2, 50); }
#[test] fn fuzz_seed_3() { run_fuzzer(3, 50); }
#[test] fn fuzz_seed_4() { run_fuzzer(4, 50); }
#[test] fn fuzz_seed_5() { run_fuzzer(5, 50); }
#[test] fn fuzz_seed_6() { run_fuzzer(6, 50); }
#[test] fn fuzz_seed_7() { run_fuzzer(7, 50); }
#[test] fn fuzz_seed_8() { run_fuzzer(8, 50); }
#[test] fn fuzz_seed_9() { run_fuzzer(9, 50); }

#[test] fn fuzz_random_seed() {
    let seed = rand::random::<u64>();
    eprintln!("FUZZER SEED: {seed}");
    run_fuzzer(seed, 100);
}

#[test] fn fuzz_heavy_rewrite() { run_fuzzer_rewrite_heavy(42, 30); }
#[test] fn fuzz_rapid_fire_checkpoints() { run_fuzzer_checkpoint_heavy(99, 80); }
```

## Taskfile Integration

```yaml
test:fuzz:
  desc: Run the attribution fuzzer
  cmds:
    - task: test:base
      vars:
        GIT_AI_TEST_GIT_MODE: daemon
        TEST_FILTER: fuzz_

test:fuzz:heavy:
  desc: Run fuzzer with high iteration count (500 ops per seed)
  cmds:
    - cargo test fuzz_ -- --test-threads 4 --nocapture
  env:
    GIT_AI_TEST_GIT_MODE: daemon
    GIT_AI_FUZZ_OPS: "500"
```

## Edge Cases to Cover

- Rapid successive checkpoints without commits (daemon batching)
- Overwriting AI lines with human edits and vice versa
- Empty file after deletions
- Single-line files
- Amend that changes attribution of existing lines
- Cherry-pick onto branch with conflicting attribution
- Rebase that replays multiple commits with mixed attribution
- Squash merge consolidating many small AI commits
- Interleaved untracked + AI + human checkpoints before a single commit

## Error Reporting Format

On failure:
```
FUZZER FAILURE (seed=42, step=23/50)
Operation: EditAndCheckpoint { attribution: Ai, lines: 3, strategy: InsertRandom }

Line 5 mismatch:
  Content: "CCCCC"
  Char: 'C' (step 3, attribution: Ai)
  Expected: Ai
  Actual blame author: "test_user" (Human)

Full operation log:
  [0] EditAndCheckpoint(Ai, 5 lines, Append) -> char 'A'
  [1] Commit
  [2] EditAndCheckpoint(KnownHuman, 3 lines, InsertRandom) -> char 'B'
  [3] EditAndCheckpoint(Ai, 3 lines, InsertRandom) -> char 'C'  <-- THIS STEP
  ...
```

## Non-Goals

- Testing non-UTF8 files (covered elsewhere)
- Testing multiple files in a single scenario (adds complexity, little new coverage)
- Testing daemon crash recovery (separate concern)
- Running in CI (explicitly excluded for now)
