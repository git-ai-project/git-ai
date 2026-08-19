---
name: git-ai-test
description: "Write, fix, or understand integration tests for the git-ai codebase. Use when asked to add tests, diagnose test failures, understand testing patterns, or figure out how to test a specific attribution/checkpoint/rebase scenario."
argument-hint: "[what you want to test or understand about the test infrastructure]"
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
---

# git-ai Test Infrastructure

The git-ai test suite uses **real git repositories** and **real subprocess invocations** of the compiled binary. There are no mocks of the git layer or the attribution engine.

## Key Principle: Assert After Every Commit

After every `commit` in every test, assert line-level attribution immediately. Never batch assertions at the end.

```rust
repo.stage_all_and_commit("Initial commit").unwrap();
file.assert_committed_lines(lines!["line 1".human(), "AI line".ai()]);
```

## TestRepo — The Test Harness

`tests/integration/repos/test_repo.rs`

```rust
let repo = TestRepo::new();                    // default mode (wrapper/daemon/wrapper-daemon per env)
let repo = TestRepo::new_dedicated_daemon();   // dedicated daemon process
let repo = TestRepo::new_worktree();           // linked worktree (returns worktree-side repo)
let (mirror, upstream) = TestRepo::new_with_remote(); // cloned pair
```

### Subprocess Methods

```rust
repo.git(&["add", "."]).unwrap();
repo.git(&["checkout", "-b", "feature"]).unwrap();
repo.git_ai(&["checkpoint", "mock_ai", "file.rs"]).unwrap();
repo.git_og(&["commit", "--author=...", "-m", "msg"]).unwrap(); // bypass git-ai hooks
repo.stage_all_and_commit("msg").unwrap();                       // returns NewCommit
repo.commit("msg").unwrap();                                     // requires manual staging
```

`git_og` uses `core.hooksPath=/dev/null` — use it when you need to set custom author/committer identity or create commits that should not trigger git-ai hooks.

### Key Accessors

```rust
repo.path()                  // &PathBuf to repo root
repo.current_branch()        // → "main"
repo.filename("foo.rs")      // → TestFile<'_>
repo.read_file("foo.rs")     // → Option<String>
repo.read_authorship_note(sha) // → Option<String> raw note text
repo.current_working_logs()  // → PersistedWorkingLog
repo.stats()                 // → CommitStats (parsed JSON from git-ai stats)
repo.patch_git_ai_config(|patch| { patch.ignore_prompts = Some(true); });
```

### Config Patching (Parallel-Safe)

Use `patch_git_ai_config` rather than env vars — it's per-test and avoids needing `#[serial_test::serial]`:

```rust
repo.patch_git_ai_config(|patch| {
    patch.exclude_prompts_in_repositories = Some(vec![]);
    patch.feature_flags = Some(serde_json::json!({"async_mode": false}));
});
```

## TestFile — The File Fluent API

`tests/integration/repos/test_file.rs`

```rust
let mut file = repo.filename("example.rs");
file.set_contents(lines!["line 1", "AI line".ai()]);
repo.stage_all_and_commit("Initial commit").unwrap();
file.assert_lines_and_blame(lines!["line 1".human(), "AI line".ai()]);
```

### lines! Macro and Attribution Types

```rust
lines![
    "plain string",                  // → .human() by default
    "Human edit".human(),            // tracked human (mock_known_human checkpoint)
    "AI wrote this".ai(),            // AI attribution (mock_ai checkpoint)
    "Untracked change".unattributed_human(),  // no checkpoint fired ("legacy human")
]
```

`AuthorType` values and their checkpoint equivalents:
| Trait method | Checkpoint preset | Meaning |
|---|---|---|
| `.ai()` | `mock_ai` | AI-attributed line |
| `.human()` | `mock_known_human` | Explicitly-known human edit |
| `.unattributed_human()` | `human` (bare) | Untracked — no checkpoint fired |

### Assertion Methods

```rust
// After a commit where working tree is clean:
file.assert_lines_and_blame(expected_lines);

// After a commit where uncommitted edits still exist in working tree:
file.assert_committed_lines(expected_lines);

// Snapshot test (uses insta):
file.assert_blame_snapshot();
```

## Two Paths for Writing File Content

### Path 1: `set_contents` (High-Level)

Use for most tests. The helper does a two-pass write: stub with placeholders → human checkpoint → real content → AI checkpoint → `git add -A`.

```rust
file.set_contents(lines!["human line", "AI line".ai()]);
repo.stage_all_and_commit("commit").unwrap();
file.assert_lines_and_blame(lines!["human line".human(), "AI line".ai()]);
```

**Do NOT use `set_contents` when:**
- You're testing checkpoint internals, ordering, or edge cases
- You need to test partial staging
- You need to replicate the exact pre/post checkpoint flow of a real agent
- The two-pass placeholder artifact would contaminate the test

### Path 2: `fs::write` + Explicit Checkpoints (Low-Level)

Use when checkpoint order and identity matter. This mirrors how real AI agents work: pre-edit (`human`) snapshot → AI edits → post-edit (`mock_ai`) snapshot.

```rust
use std::fs;

let file_path = repo.path().join("example.rs");

// Commit 1: completely untracked (no checkpoint at all)
fs::write(&file_path, "Untracked line\n").unwrap();
repo.stage_all_and_commit("Initial commit").unwrap();
let mut file = repo.filename("example.rs");
file.assert_committed_lines(lines!["Untracked line".unattributed_human()]);

// Commit 2: explicit known-human checkpoint
fs::write(&file_path, "Untracked line\nHuman line\n").unwrap();
repo.git_ai(&["checkpoint", "mock_known_human", "example.rs"]).unwrap();
repo.git(&["add", "."]).unwrap();
repo.commit("Second commit").unwrap();
file.assert_committed_lines(lines![
    "Untracked line".unattributed_human(),
    "Human line".human(),
]);

// Commit 3: AI agent pre/post checkpoint pair
// Step A: pre-edit snapshot (mimics AI agent preset firing before it edits)
fs::write(&file_path, "Untracked line\nHuman line\nPre-existing line\n").unwrap();
repo.git_ai(&["checkpoint", "human", "example.rs"]).unwrap();  // "legacy" untracked pre-snapshot
// Step B: AI edits the file
fs::write(&file_path, "Untracked line\nHuman line\nPre-existing line\nAI Line\n").unwrap();
// Step C: post-edit AI checkpoint
repo.git_ai(&["checkpoint", "mock_ai", "example.rs"]).unwrap();
repo.stage_all_and_commit("Third commit").unwrap();
file.assert_committed_lines(lines![
    "Untracked line".unattributed_human(),
    "Human line".human(),
    "Pre-existing line".unattributed_human(),
    "AI Line".ai(),
]);
```

**Valid checkpoint preset names:** `claude`, `codex`, `continue-cli`, `cursor`, `gemini`, `github-copilot`, `amp`, `windsurf`, `opencode`, `pi`, `ai_tab`, `firebender`, `mock_ai`, `mock_known_human`, `known_human`, `droid`, `agent-v1`.

**Scoped vs unscoped checkpoints:**
```rust
repo.git_ai(&["checkpoint", "mock_ai", "file.rs"]).unwrap();  // scoped to one file
repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();              // unscoped: all dirty files
```

## Test Variant Macros

At the end of most test files, add macro calls to double coverage via worktree-backed repos:

```rust
crate::reuse_tests_in_worktree!(
    test_basic_attribution,
    test_ai_line_persists,
);
```

For tests that exercise repo-discovery or path handling, use `subdir_test_variants!` to generate 4 variants (plain, worktree, `-C flag`, `-C flag + worktree`):

```rust
crate::subdir_test_variants! {
    fn test_something_from_subdir() {
        let repo = TestRepo::new();
        // ... test body ...
    }
}
```

## Test Isolation

Every `TestRepo` gets isolated:
- Random temp directory
- Isolated `HOME` and `~/.gitconfig`
- Per-test SQLite DB path (`GIT_AI_TEST_DB_PATH`) as a sibling of the repo dir
- `GIT_AI_TEST_CONFIG_PATCH` env var for config overrides

Use `#[serial_test::serial]` only when a test must mutate process-global env vars that other tests could race on. Avoid it by using `patch_git_ai_config` instead.

Use `#[ignore]` for:
- Benchmarks (run with `task test EXTRA_TEST_BINARY_ARGS="--ignored"`)
- Network-dependent tests
- Tests known-broken with a TODO explaining when to fix

## Testing Rebase / Cherry-Pick / Stash Attribution

### Rebase

```rust
let repo = TestRepo::new();
let mut file = repo.filename("foo.rs");
file.set_contents(lines!["base"]);
repo.stage_all_and_commit("Initial").unwrap();
let main = repo.current_branch();

repo.git(&["checkout", "-b", "feature"]).unwrap();
let mut f = repo.filename("feature.rs");
f.set_contents(lines!["AI line".ai()]);
repo.stage_all_and_commit("AI feature").unwrap();

repo.git(&["checkout", &main]).unwrap();
// advance main...
repo.git(&["checkout", "feature"]).unwrap();
repo.git(&["rebase", &main]).unwrap();

// Assert attribution survives rebase
f.assert_lines_and_blame(lines!["AI line".ai()]);
```

### Stash

```rust
let mut file = repo.filename("work.rs");
file.set_contents(lines!["AI work".ai()]);
repo.git_ai(&["checkpoint", "mock_ai"]).unwrap(); // unscoped
repo.git(&["stash"]).unwrap();
// working tree is clean
repo.git(&["stash", "pop"]).unwrap();
let commit = repo.stage_all_and_commit("apply stash").unwrap();
file.assert_lines_and_blame(lines!["AI work".ai()]);
assert!(!commit.authorship_log.metadata.prompts.is_empty());
```

### Cherry-pick

```rust
repo.git(&["checkout", "-b", "other"]).unwrap();
// make commits, get sha
repo.git(&["checkout", &main]).unwrap();
repo.git(&["cherry-pick", &feature_sha]).unwrap();
// assert attribution transferred to new commit
```

## Verifying the Authorship Log Directly

When blame isn't enough, read the raw note:

```rust
let commit = repo.stage_all_and_commit("msg").unwrap();
let log = &commit.authorship_log;
assert!(!log.metadata.prompts.is_empty());
assert_eq!(log.attestations.len(), 1);
assert!(log.attestations[0].entries.iter().any(|e| !e.hash.starts_with("h_")));
```

## Running Tests

```bash
task test                                    # full suite
task test TEST_FILTER=test_basic_attribution # specific test
task test NO_CAPTURE=true                    # with stdout
task test EXTRA_TEST_BINARY_ARGS="--ignored" # include ignored (benchmarks)
```

## What Makes a Good Attribution Test

1. **Assert line-level after every commit** — use `assert_lines_and_blame` or `assert_committed_lines`
2. **Choose the right writing path** — `set_contents` for simple scenarios, `fs::write` + explicit checkpoints for checkpoint-internals tests
3. **Verify the authorship log, not just blame** — check `commit.authorship_log.metadata.prompts` and `attestations`
4. **Add `reuse_tests_in_worktree!`** at the bottom of the file for free worktree coverage
5. **Use `patch_git_ai_config`** rather than env vars to stay parallel-safe
6. **Keep `#[serial_test::serial]` out** unless truly needed (process-global env mutation)
