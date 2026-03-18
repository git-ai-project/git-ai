# Test Binary Consolidation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate 92 separate integration test binaries into a single binary to eliminate ~90% of link time and ~80% of disk usage.

**Architecture:** Move all `tests/*.rs` files into `tests/integration/` as submodules of a single `tests/integration/main.rs` crate root. Shared modules (`repos/`, `test_utils.rs`) are declared once at the crate root. Replace the runtime `compile_binary()` function with the compile-time `env!("CARGO_BIN_EXE_git-ai")` macro. Regenerate all 122 insta snapshots under the new module paths.

**Tech Stack:** Rust, Cargo `[[test]]` sections, insta snapshots, serial_test, paste (macro crate)

---

## Chunk 1: Create Single Test Binary Structure

### Task 1: Create the integration test directory and main.rs

**Files:**
- Create: `tests/integration/main.rs`

- [ ] **Step 1: Create the directory**

```bash
mkdir -p tests/integration
```

- [ ] **Step 2: Write `main.rs` with all module declarations**

The crate root must:
1. Declare `repos` and `test_utils` as shared modules (with `#[macro_use]` on `repos` so the macros `subdir_test_variants!`, `worktree_test_wrappers!`, `reuse_tests_in_worktree!`, `reuse_tests_in_worktree_with_attrs!` are available to all submodules)
2. Declare every test file as a module

```rust
// Shared infrastructure — declared once for all test modules
#[macro_use]
mod repos;
mod test_utils;

// Test modules (one per original test file)
mod agent_commits_blame;
mod agent_presets_comprehensive;
mod agent_v1;
mod ai_tab;
mod amend;
mod amp;
mod attribution_tracker_comprehensive;
mod blame_comprehensive;
mod blame_flags;
mod blame_subdirectory;
mod checkout_hooks_comprehensive;
mod checkout_switch;
mod checkpoint_size;
mod cherry_pick;
mod cherry_pick_hooks_comprehensive;
mod chinese_text_edits;
mod ci_handlers_comprehensive;
mod ci_local_skip_fetch;
mod ci_squash_rebase;
mod claude_code;
mod codex;
mod commit_hooks_comprehensive;
mod commit_post_stats_benchmark;
mod config_pattern_detection;
mod continue_cli;
mod continue_session;
mod cross_repo_cwd_attribution;
mod cursor;
mod diff;
mod diff_comprehensive;
mod diff_ignore_binary;
mod droid;
mod e2big_post_filter;
mod gemini;
mod git_alias_resolution;
mod git_cli_arg_parsing;
mod git_repository_comprehensive;
mod github_copilot;
mod github_copilot_integration;
mod github_integration;
mod gix_config_tests;
mod graphite;
mod hook_forwarding;
mod hook_modes;
mod hooks_feature_flags;
mod ignore_prompts;
mod initial_attributions;
mod install_hooks_comprehensive;
mod internal_db_integration;
mod internal_machine_commands;
mod internal_spawn_safety;
mod jetbrains_download;
mod jetbrains_ide_types;
mod merge_hooks_comprehensive;
mod merge_rebase;
mod multi_repo_workspace;
mod non_utf8_files;
mod observability_flush;
mod opencode;
mod performance;
mod prompt_across_commit;
mod prompt_hash_migration;
mod prompt_picker_test;
mod prompts_db_test;
mod pull_rebase_ff;
mod push_upstream_authorship;
mod realistic_complex_edits;
mod rebase;
mod rebase_hooks_comprehensive;
mod reset;
mod reset_hooks_comprehensive;
mod search;
mod secrets_benchmark;
mod share_tui_comprehensive;
mod show_prompt;
mod simple_additions;
mod simple_benchmark;
mod squash_merge;
mod stash_attribution;
mod stats;
mod status_ignore;
mod subdirs;
mod sublime_merge_installer;
mod switch_hooks_comprehensive;
mod sync_authorship_types;
mod tls_native_certs;
mod utf8_filenames;
mod virtual_attribution_merge;
mod windsurf;
mod worktrees;
mod wrapper_performance_targets;
```

- [ ] **Step 3: Commit**

```bash
git add tests/integration/main.rs
git commit -m "test: add integration test crate root with all module declarations"
```

### Task 2: Move shared infrastructure into integration/

**Files:**
- Move: `tests/repos/` → `tests/integration/repos/`
- Move: `tests/test_utils.rs` → `tests/integration/test_utils.rs`
- Move: `tests/snapshots/` → `tests/integration/snapshots/`

- [ ] **Step 1: Move repos directory**

```bash
mv tests/repos tests/integration/repos
```

- [ ] **Step 2: Move test_utils.rs**

```bash
mv tests/test_utils.rs tests/integration/test_utils.rs
```

- [ ] **Step 3: Move snapshots directory**

```bash
mv tests/snapshots tests/integration/snapshots
```

- [ ] **Step 4: Remove crate-level attributes from moved files**

In `tests/integration/repos/test_repo.rs`, `tests/integration/repos/test_file.rs`, and `tests/integration/test_utils.rs`, change `#![allow(dead_code)]` (crate-level) to `#[allow(dead_code)]` (module-level) by removing the `!`. Actually, since these are now submodules of the integration crate, the `#![allow(dead_code)]` becomes invalid. Instead, add `#[allow(dead_code)]` to the `mod` declarations in `main.rs`:

In `tests/integration/main.rs`, change:
```rust
mod repos;
```
to:
```rust
#[allow(dead_code)]
mod repos;
```
And:
```rust
mod test_utils;
```
to:
```rust
#[allow(dead_code)]
mod test_utils;
```

Then remove the `#![allow(dead_code)]` line from `tests/integration/repos/test_repo.rs`, `tests/integration/repos/test_file.rs`, and `tests/integration/test_utils.rs`.

- [ ] **Step 5: Commit**

```bash
git add -A tests/integration/repos tests/integration/test_utils.rs tests/integration/snapshots
git add tests/repos tests/test_utils.rs tests/snapshots  # stage deletions
git commit -m "test: move shared infrastructure into integration/ directory"
```

### Task 3: Move test files and strip duplicate mod declarations

**Files:**
- Move: all 91 `tests/*.rs` files (excluding `test_utils.rs`, already moved) → `tests/integration/`

Each file needs these mechanical edits:
1. Remove any `#[macro_use]` line immediately before `mod repos;`
2. Remove the `mod repos;` line
3. Remove any `mod test_utils;` line
4. Replace `use repos::` with `use super::repos::` or `use crate::repos::`
5. Replace `use test_utils::` with `use super::test_utils::` or `use crate::test_utils::`

**Important macro detail:** The macros `subdir_test_variants!`, `worktree_test_wrappers!`, `reuse_tests_in_worktree!`, and `reuse_tests_in_worktree_with_attrs!` use `#[macro_export]` which puts them at the crate root. They reference `$crate::repos::test_repo::...` internally. After consolidation, `$crate` resolves to the integration crate root, where `repos` is declared — so these macro paths work correctly without changes.

- [ ] **Step 1: Move all test files**

```bash
# Move all .rs files except test_utils.rs (already moved)
for f in tests/*.rs; do
    [ "$(basename "$f")" = "test_utils.rs" ] && continue
    mv "$f" tests/integration/
done
```

- [ ] **Step 2: Strip `mod repos;` and `mod test_utils;` declarations from all moved files**

For each file in `tests/integration/*.rs` (excluding `main.rs`, `test_utils.rs`), apply these edits:

```bash
# Remove lines matching: #[macro_use] followed by mod repos; or just mod repos; or mod test_utils;
cd tests/integration
for f in *.rs; do
    [ "$f" = "main.rs" ] && continue
    [ "$f" = "test_utils.rs" ] && continue
    # Use sed to remove the lines (in-place)
    sed -i '' '/^#\[macro_use\]$/{N;/\nmod repos;/d;}' "$f"
    sed -i '' '/^mod repos;$/d' "$f"
    sed -i '' '/^mod test_utils;$/d' "$f"
done
cd ../..
```

- [ ] **Step 3: Fix import paths — `use repos::` → `use crate::repos::`**

Since `repos` is declared at the crate root (in `main.rs`), submodules must use `crate::repos::` or `super::repos::`:

```bash
cd tests/integration
for f in *.rs; do
    [ "$f" = "main.rs" ] && continue
    [ "$f" = "test_utils.rs" ] && continue
    sed -i '' 's/^use repos::/use crate::repos::/g' "$f"
    sed -i '' 's/^use test_utils::/use crate::test_utils::/g' "$f"
done
cd ../..
```

- [ ] **Step 4: Commit**

```bash
git add -A tests/integration/ tests/*.rs
git commit -m "test: move all test files into integration/ and fix module paths"
```

### Task 4: Configure Cargo.toml for single test binary

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `[[test]]` section to Cargo.toml**

Append to `Cargo.toml`:

```toml
[[test]]
name = "integration"
path = "tests/integration/main.rs"
harness = true
```

- [ ] **Step 2: Verify no stale test files remain in tests/**

```bash
ls tests/*.rs  # Should show no files (all moved)
```

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "test: configure single integration test binary in Cargo.toml"
```

### Task 5: Fix compilation — first pass

- [ ] **Step 1: Attempt compilation**

```bash
cargo test --no-run 2>&1 | head -100
```

- [ ] **Step 2: Fix any remaining import/path issues**

Common issues to expect:
- `use repos::test_file::...` → `use crate::repos::test_file::...`
- `use repos::test_repo::...` → `use crate::repos::test_repo::...`
- Any `use test_utils::...` → `use crate::test_utils::...`
- Inner modules using relative paths may need adjustment

Iterate until `cargo test --no-run` succeeds.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "test: fix compilation issues after consolidation"
```

## Chunk 2: Replace compile_binary() and Regenerate Snapshots

### Task 6: Replace `compile_binary()` with `CARGO_BIN_EXE`

**Files:**
- Modify: `tests/integration/repos/test_repo.rs`

- [ ] **Step 1: Replace the compile_binary/get_binary_path functions**

In `tests/integration/repos/test_repo.rs`, replace:
```rust
static COMPILED_BINARY: OnceLock<PathBuf> = OnceLock::new();
```
and the `compile_binary()` function (lines ~1270-1322) and `get_binary_path()` function (lines ~1324-1326) with:

```rust
pub fn get_binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_git-ai")
}
```

- [ ] **Step 2: Update call sites**

The return type changes from `&'static PathBuf` to `&'static str`. Search for all uses of `get_binary_path()` and update:
- `Command::new(get_binary_path())` works with both `&PathBuf` and `&str`, so most call sites should be fine
- If any code calls `.as_path()` or other `PathBuf`-specific methods, wrap with `Path::new(get_binary_path())`

- [ ] **Step 3: Remove unused imports**

Remove `OnceLock` import if no longer used. Remove `Command` import from the `compile_binary` context if no longer needed elsewhere.

- [ ] **Step 4: Verify it compiles**

```bash
cargo test --no-run
```

- [ ] **Step 5: Commit**

```bash
git add tests/integration/repos/test_repo.rs
git commit -m "test: replace runtime cargo build with compile-time CARGO_BIN_EXE"
```

### Task 7: Regenerate insta snapshots

**Files:**
- Modify: all files in `tests/integration/snapshots/` and `tests/integration/repos/snapshots/`

After consolidation, `insta` snapshot names change because the crate name changes from individual test file names (e.g., `blame_flags`) to `integration`. The module path also gains one level (e.g., `blame_flags::test_fn` → `integration::blame_flags::test_fn`).

The snapshot file names change from:
- `blame_flags__blame_multiple_flags.snap` → `integration__blame_flags__blame_multiple_flags.snap`
- `simple_additions__ai_adds_lines_with_unstaged_modifications-2.snap` → `integration__simple_additions__ai_adds_lines_with_unstaged_modifications-2.snap`

The snapshot CONTENT should be identical — only the names change.

- [ ] **Step 1: Delete old snapshot files**

```bash
rm -rf tests/integration/snapshots/*.snap
rm -rf tests/integration/repos/snapshots/*.snap
```

- [ ] **Step 2: Run tests to regenerate snapshots**

```bash
cargo insta test --accept -- --test integration
```

If `cargo-insta` is not installed:
```bash
cargo install cargo-insta
cargo insta test --accept -- --test integration
```

Alternatively, run tests with `INSTA_UPDATE=always`:
```bash
INSTA_UPDATE=always cargo test --test integration
```

- [ ] **Step 3: Verify snapshot content matches original**

Spot-check a few regenerated snapshots against the git history to confirm content is identical (only filenames changed).

- [ ] **Step 4: Commit**

```bash
git add tests/integration/snapshots/ tests/integration/repos/snapshots/
git commit -m "test: regenerate insta snapshots with consolidated module paths"
```

### Task 8: Full verification

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --test integration
```

All tests should pass. The test count should match the original count.

- [ ] **Step 2: Count tests to verify none were lost**

```bash
# Before consolidation (run on main branch for comparison):
# cargo test 2>&1 | grep "test result"
# After consolidation:
cargo test --test integration 2>&1 | grep "test result"
```

The total number of tests should be identical.

- [ ] **Step 3: Verify disk space improvement**

```bash
# Clean and rebuild
cargo clean
cargo test --test integration --no-run
du -sh target/
```

Compare with the old approach (92 binaries). Expected: ~80% reduction in target/ size.

- [ ] **Step 4: Commit any remaining fixes**

```bash
git add -A
git commit -m "test: final fixes for consolidated test binary"
```

## Notes

### Why this works

- Cargo treats each `.rs` file in `tests/` as a separate integration test crate (separate binary)
- Each binary must link against all dependencies (~50 MB per binary × 92 = ~4.5 GB)
- Each binary re-compiles `mod repos` (2,527 lines) from scratch
- A single binary compiles and links everything once

### What changes for developers

- `cargo test` runs all tests from a single binary (faster compilation, same test behavior)
- Individual tests can still be run by name: `cargo test --test integration test_name`
- `serial_test::serial` still works within a single binary (same-process serialization)
- The `OnceLock`-based `COMPILED_BINARY` (replaced by `CARGO_BIN_EXE`) is resolved at compile time, zero runtime cost

### Risk assessment

- **Snapshot regeneration**: Names change but content is identical. Verified by spot-checking.
- **Test isolation**: Tests already share a process when using `--test-threads=1`. The `OnceLock` for binary path actually benefits from single-binary (initialized once, shared).
- **Global state**: 3 test files use `serial_test::serial` for test serialization — this continues to work correctly within a single binary.
- **Macro paths**: `$crate::repos::...` in `#[macro_export]` macros resolves to the integration crate root where `repos` is declared — no changes needed.
