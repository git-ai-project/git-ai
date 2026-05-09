# Drop Legacy Synchronous Wrapper Mode

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the legacy synchronous wrapper mode and async_mode feature flag, leaving only daemon and wrapper-daemon modes.

**Architecture:** The codebase currently has three git-proxy operating modes: synchronous wrapper (legacy), daemon, and wrapper-daemon. We're removing the synchronous wrapper mode entirely, since all users are on daemon or wrapper-daemon. The `async_mode` feature flag that gates the dispatch becomes unnecessary — the async path is always taken. Dead code cascades from `run_pre/post_command_hooks`, the synchronous hook entry points, `wrapper_performance_targets::log_performance_target_if_violated`, and the `GitTestMode::Wrapper`/`Hooks`/`Both` test variants.

**Tech Stack:** Rust 2024 edition, GitHub Actions CI, Taskfile

---

## Key Constraints

- **Keep daemon-shared hook utilities**: `push_hooks::run_pre_push_hook_managed()`, `rebase_hooks::build_rebase_commit_mappings()`, `stash_hooks::{save_stash_authorship_log, restore_stash_attributions, extract_stash_pathspecs}`, and `plumbing_rewrite_hooks::apply_wrapper_plumbing_rewrite_if_possible()` are used by the daemon. Do NOT remove these.
- **Keep `log_performance_for_checkpoint`**: This function in `wrapper_performance_targets.rs` is used by `git_ai_handlers.rs` for checkpoint timing. Only remove `log_performance_target_if_violated` and `BenchmarkResult`.
- **Tests using `GitTestMode::Wrapper` in daemon_mode.rs and async_mode.rs**: These create bare repos (no daemon configured) for daemon-specific tests. They need `GitTestMode::Daemon` with `DaemonTestScope::Dedicated` since they manually manage daemons.
- **Compile-check after each task** to catch dependency issues early.

---

### Task 1: Remove `async_mode` feature flag

**Files:**
- Modify: `src/feature_flags.rs`

- [ ] **Step 1: Remove `async_mode` from the `define_feature_flags!` macro**

In `src/feature_flags.rs`, remove the `async_mode` line from the macro invocation:

```rust
// REMOVE this line:
async_mode: async_mode, debug = false, release = true,
```

The `define_feature_flags!` macro at lines 54-61 becomes:

```rust
define_feature_flags!(
    rewrite_stash: rewrite_stash, debug = true, release = true,
    auth_keyring: auth_keyring, debug = false, release = false,
    git_hooks_enabled: git_hooks_enabled, debug = false, release = false,
    git_hooks_externally_managed: git_hooks_externally_managed, debug = false, release = false,
);
```

- [ ] **Step 2: Remove the `git_hooks_enabled → async_mode` migration in `from_env_and_file`**

In `src/feature_flags.rs` function `from_env_and_file` (lines 110-114), remove:

```rust
        // Git core hooks have been sunset — users who had hooks enabled are
        // migrated to async (daemon) mode automatically.
        if result.git_hooks_enabled {
            result.async_mode = true;
        }
```

- [ ] **Step 3: Update tests in `feature_flags.rs` to remove all `async_mode` references**

Remove `async_mode` assertions from `test_default_feature_flags`, remove `async_mode` fields from all test structs (e.g. `test_from_env_and_file_file_overrides`, `test_serialization`, `test_clone_trait`), and remove `GIT_AI_ASYNC_MODE` env var cleanup from serial tests.

- [ ] **Step 4: Compile check**

Run: `cargo check 2>&1 | head -80`

This will produce errors everywhere `async_mode` is referenced — that's expected and guides the next tasks.

- [ ] **Step 5: Commit**

```bash
git add src/feature_flags.rs
git commit -m "refactor: remove async_mode feature flag from FeatureFlags"
```

---

### Task 2: Simplify `git_handlers.rs` — remove synchronous wrapper branch

**Files:**
- Modify: `src/commands/git_handlers.rs`

- [ ] **Step 1: Remove wrapper-only imports**

Remove these imports that are only used in the synchronous wrapper branch:

```rust
use crate::authorship::virtual_attribution::VirtualAttributions;
use crate::commands::hooks::checkout_hooks;
use crate::commands::hooks::cherry_pick_hooks;
use crate::commands::hooks::clone_hooks;
use crate::commands::hooks::commit_hooks;
use crate::commands::hooks::fetch_hooks;
use crate::commands::hooks::merge_hooks;
use crate::commands::hooks::push_hooks;
use crate::commands::hooks::rebase_hooks;
use crate::commands::hooks::reset_hooks;
use crate::commands::hooks::stash_hooks;
use crate::commands::hooks::switch_hooks;
use crate::commands::hooks::update_ref_hooks;
use crate::observability::wrapper_performance_targets::log_performance_target_if_violated;
```

Keep the `config`, `git::cli_parser`, `git::find_repository`, `git::repository`, `observability` imports that the async path uses.

- [ ] **Step 2: Remove `CommandHooksContext` struct (lines 91-101)**

This struct is only used by `run_pre/post_command_hooks`. Remove it entirely.

- [ ] **Step 3: Remove `HookPanicError` struct (lines 48-57)**

Only used by the panic-catching wrappers in `run_pre/post_command_hooks`. Remove it entirely.

- [ ] **Step 4: Inline the async mode path — remove the `if async_mode` conditional**

In `handle_git()` (line 103), the current structure is:

```rust
if config::Config::get().feature_flags().async_mode {
    // async path (lines 115-193)
    ...
    exit_with_status(exit_status);
}

// synchronous wrapper path (lines 196-288)
...
```

Remove the `if` conditional and make the async path the only path. Remove the entire synchronous wrapper path (lines 196-288). The function body after the shell completion check becomes just the async path code (currently lines 115-193), without the `if` wrapper.

- [ ] **Step 5: Remove `run_pre_command_hooks` function (lines 416-493)**

Delete the entire function — it's only called from the removed synchronous path.

- [ ] **Step 6: Remove `run_post_command_hooks` function (lines 495-598)**

Delete the entire function — it's only called from the removed synchronous path.

- [ ] **Step 7: Clean up the `proxy_to_git` function**

In the `proxy_to_git` function, remove the `env_remove("GIT_AI_ASYNC_MODE")` calls (lines 858 and 892) since the env var no longer exists.

- [ ] **Step 8: Compile check**

Run: `cargo check 2>&1 | head -80`

- [ ] **Step 9: Commit**

```bash
git add src/commands/git_handlers.rs
git commit -m "refactor: remove synchronous wrapper dispatch path from git_handlers"
```

---

### Task 3: Remove `async_mode` checks from `git_ai_handlers.rs`, `install_hooks.rs`, `utils.rs`

**Files:**
- Modify: `src/commands/git_ai_handlers.rs`
- Modify: `src/commands/install_hooks.rs`
- Modify: `src/utils.rs`

- [ ] **Step 1: Simplify `git_ai_handlers.rs` — always init daemon telemetry**

At lines 42-77, remove the `if config::Config::get().feature_flags().async_mode` wrapper. The daemon telemetry init logic should always run (the `needs_daemon` check and the `init_daemon_telemetry_handle` call). Keep the `needs_daemon` exclusion list and the error handling.

- [ ] **Step 2: Simplify `install_hooks.rs` — `maybe_configure_async_mode_daemon_trace2`**

In `maybe_configure_async_mode_daemon_trace2` (line 241), remove the early return when `async_mode` is false (lines 244-249). The function should always configure trace2. Rename to `configure_daemon_trace2`.

- [ ] **Step 3: Simplify `install_hooks.rs` — `maybe_teardown_async_mode`**

Delete the `maybe_teardown_async_mode` function entirely (lines 279-302). It only tears down daemons when async_mode is off — since async_mode is always on, this is dead code. Remove the call to it from `run()` (line 355).

- [ ] **Step 4: Simplify `install_hooks.rs` — `maybe_ensure_daemon`**

In `maybe_ensure_daemon` (line 304), remove the early return when `async_mode` is false (lines 310-312). Rename to `ensure_daemon`.

- [ ] **Step 5: Simplify `install_hooks.rs` — `run()` function**

Remove the `if config::Config::get().feature_flags().async_mode` check around the telemetry handle init (line 360). Always init the handle.

- [ ] **Step 6: Simplify `utils.rs` — `checkpoint_delegation_enabled`**

In `checkpoint_delegation_enabled()` (lines 55-67), remove the `async_mode` check. The function now just checks the `GIT_AI_DAEMON_CHECKPOINT_DELEGATE` env var fallback. Or if the intent is "always true when async_mode is on", simplify to always return true with the env var as an override.

Actually, looking at the function more carefully: since async_mode is always true, the function would always return true before reaching the env var check. Simplify the whole function to just `return true;` but keep the function signature for the callers. Better: just inline `true` at call sites if there are few. Check the call sites first.

- [ ] **Step 7: Compile check**

Run: `cargo check 2>&1 | head -80`

- [ ] **Step 8: Commit**

```bash
git add src/commands/git_ai_handlers.rs src/commands/install_hooks.rs src/utils.rs
git commit -m "refactor: remove async_mode conditionals from git_ai_handlers, install_hooks, utils"
```

---

### Task 4: Clean up `wrapper_performance_targets.rs`

**Files:**
- Modify: `src/observability/wrapper_performance_targets.rs`

- [ ] **Step 1: Remove `log_performance_target_if_violated` and `BenchmarkResult`**

Remove the `log_performance_target_if_violated` function (lines 19-94) and the `BenchmarkResult` struct (lines 11-17). Keep `log_performance_for_checkpoint` (lines 96-144) and `PERFORMANCE_FLOOR_MS` only if it's used by the remaining code (check — it's not used by `log_performance_for_checkpoint`, so remove it too).

- [ ] **Step 2: Remove dead tests**

Remove all tests in the `#[cfg(test)] mod tests` block that test the removed functions. Keep only the `log_performance_for_checkpoint` tests.

- [ ] **Step 3: Rename the file**

Since this is no longer wrapper-specific, rename from `wrapper_performance_targets.rs` to `performance_targets.rs`. Update the `pub mod` in `src/observability/mod.rs` and all import sites (`src/commands/git_ai_handlers.rs` line 20, `tests/integration/repos/test_repo.rs` line 14, `tests/integration/performance.rs` line 12).

- [ ] **Step 4: Compile check**

Run: `cargo check 2>&1 | head -80`

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: remove wrapper-only performance targets, rename to performance_targets"
```

---

### Task 5: Remove dead wrapper-only hook entry points

**Files:**
- Modify: Multiple files in `src/commands/hooks/`

- [ ] **Step 1: Identify which pre/post hook functions are dead**

For each hook module, check if the daemon uses any of its functions. The daemon uses:
- `push_hooks::run_pre_push_hook_managed()` — KEEP
- `rebase_hooks::build_rebase_commit_mappings()` — KEEP
- `stash_hooks::{save_stash_authorship_log, restore_stash_attributions, extract_stash_pathspecs}` — KEEP
- `plumbing_rewrite_hooks::apply_wrapper_plumbing_rewrite_if_possible()` — KEEP

All `pre_*_hook()` and `post_*_hook()` functions called from the now-deleted `run_pre_command_hooks` / `run_post_command_hooks` are dead code. Remove them.

- [ ] **Step 2: For each hook module, remove dead functions**

For modules where ALL functions are dead (no daemon usage), the entire module file can be emptied or the module can be removed. Modules with mixed usage keep only the daemon-used functions.

Likely entirely dead modules (remove content or delete file + remove from mod.rs):
- `checkout_hooks.rs` — no daemon usage
- `clone_hooks.rs` — no daemon usage
- `fetch_hooks.rs` — no daemon usage  
- `merge_hooks.rs` — no daemon usage
- `switch_hooks.rs` — no daemon usage
- `update_ref_hooks.rs` — no daemon usage

Partially dead modules (remove only wrapper entry points):
- `commit_hooks.rs` — remove `commit_pre_command_hook`, `commit_post_command_hook`; check if anything else remains
- `push_hooks.rs` — remove `push_pre_command_hook`, `push_post_command_hook`; KEEP `run_pre_push_hook_managed`
- `rebase_hooks.rs` — remove `pre_rebase_hook`, `handle_rebase_post_command`; KEEP `build_rebase_commit_mappings`
- `reset_hooks.rs` — remove `pre_reset_hook`, `post_reset_hook`; check for daemon usage
- `cherry_pick_hooks.rs` — remove `pre_cherry_pick_hook`, `post_cherry_pick_hook`; check for daemon usage
- `stash_hooks.rs` — remove `pre_stash_hook`, `post_stash_hook`; KEEP daemon utilities

IMPORTANT: Before deleting, grep each function to ensure it's truly unused. The daemon imports are the ground truth.

- [ ] **Step 3: Update `src/commands/hooks/mod.rs`**

Remove `pub mod` entries for fully deleted modules.

- [ ] **Step 4: Compile check**

Run: `cargo check 2>&1 | head -80`

The compiler will catch any function we removed that was actually still needed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: remove dead wrapper-only hook entry points"
```

---

### Task 6: Remove `GitTestMode::Wrapper`, `Hooks`, `Both` from test infrastructure

**Files:**
- Modify: `tests/integration/repos/test_repo.rs`
- Modify: `tests/integration/repos/mod.rs`

- [ ] **Step 1: Remove enum variants from `GitTestMode`**

In `tests/integration/repos/test_repo.rs` lines 42-49, remove `Wrapper`, `Hooks`, `Both` variants:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTestMode {
    Daemon,
    WrapperDaemon,
}
```

- [ ] **Step 2: Update `from_env` and `from_mode_name`**

Update `from_env()` to default to `"daemon"` instead of `"wrapper"`. Update `from_mode_name()` to map unknown modes to `Daemon`. Remove the hooks/both/wrapper match arms.

```rust
impl GitTestMode {
    pub fn from_env() -> Self {
        let mode = std::env::var("GIT_AI_TEST_GIT_MODE")
            .unwrap_or_else(|_| "daemon".to_string())
            .to_lowercase();
        Self::from_mode_name(&mode)
    }

    pub fn from_mode_name(mode: &str) -> Self {
        match mode.to_lowercase().as_str() {
            "daemon" | "trace-daemon" | "pure-daemon" => Self::Daemon,
            "wrapper-daemon" => Self::WrapperDaemon,
            _ => Self::Daemon,
        }
    }
```

- [ ] **Step 3: Simplify mode predicates**

```rust
    pub fn uses_wrapper(self) -> bool {
        matches!(self, Self::WrapperDaemon)
    }

    pub fn uses_hooks(self) -> bool {
        false
    }

    pub fn uses_daemon(self) -> bool {
        true  // Both remaining modes use daemon
    }
}
```

- [ ] **Step 4: Update `apply_default_config_patch`**

In `apply_default_config_patch` (line 909), remove the `async_mode` config patch for WrapperDaemon since async_mode no longer exists:

```rust
    fn apply_default_config_patch(&mut self) {
        self.patch_git_ai_config(|patch| {
            patch.exclude_prompts_in_repositories = Some(vec![]);
            patch.prompt_storage = Some("notes".to_string());
        });
    }
```

- [ ] **Step 5: Update `worktree_test_wrappers` macro in `mod.rs`**

In `tests/integration/repos/mod.rs`, the `worktree_test_wrappers!` macro creates three variants: `_in_worktree_wrapper_mode`, `_in_worktree_daemon_mode`, `_in_worktree_wrapper_daemon_mode`. Remove the `_in_worktree_wrapper_mode` variant (lines 323-364) since `GitTestMode::Wrapper` no longer exists.

- [ ] **Step 6: Compile check**

Run: `cargo check --tests 2>&1 | head -80`

- [ ] **Step 7: Commit**

```bash
git add tests/integration/repos/
git commit -m "refactor: remove Wrapper/Hooks/Both from GitTestMode enum"
```

---

### Task 7: Update tests that explicitly use `GitTestMode::Wrapper`

**Files:**
- Modify: `tests/async_mode.rs`
- Modify: `tests/daemon_mode.rs`
- Modify: `tests/windows_install_script.rs`
- Modify: `tests/notes_sync_regression.rs`
- Modify: `tests/integration/performance.rs`

- [ ] **Step 1: Update `tests/async_mode.rs`**

Most tests here use `TestRepo::new_with_mode(GitTestMode::Wrapper)` to create bare repos for daemon testing. These need to use `GitTestMode::Daemon` with `DaemonTestScope::Dedicated` since they manually start/manage daemons. 

The first test `async_mode_wrapper_commit_passthrough_skips_git_ai_side_effects` is specifically testing the old wrapper+async_mode behavior — this test is entirely dead and should be deleted.

For daemon infrastructure tests (like `install_hooks_async_mode_*`, `daemon_run_survives_*`, etc.), change `GitTestMode::Wrapper` to `GitTestMode::Daemon` and use `DaemonTestScope::Dedicated` where the test manually manages its own daemon.

Review each test carefully — some may be entirely about testing the async_mode toggle and should be deleted, while others test daemon behavior that's still relevant.

- [ ] **Step 2: Update `tests/daemon_mode.rs`**

All ~30 uses of `GitTestMode::Wrapper` here are for daemon tests that manually manage daemon processes. Change to `GitTestMode::Daemon` with `DaemonTestScope::Dedicated`.

- [ ] **Step 3: Update `tests/windows_install_script.rs`**

Change `GitTestMode::Wrapper` references to `GitTestMode::Daemon`.

- [ ] **Step 4: Update `tests/notes_sync_regression.rs`**

The match arm at line 464 includes `GitTestMode::Wrapper | GitTestMode::Both` — remove those match arms.

- [ ] **Step 5: Update `tests/integration/performance.rs`**

Remove the `async_mode: false` config patch at line 22. Update `BenchmarkResult` references if the type was renamed/moved in Task 4.

- [ ] **Step 6: Update `tests/integration/wrapper_performance_targets.rs`**

Update imports to reference the renamed module (`performance_targets` instead of `wrapper_performance_targets`). Remove tests for deleted types/functions.

- [ ] **Step 7: Compile and test check**

Run: `cargo check --tests 2>&1 | head -80`
Then: `task test TEST_FILTER=async_mode`

- [ ] **Step 8: Commit**

```bash
git add tests/
git commit -m "refactor: update test files to remove GitTestMode::Wrapper references"
```

---

### Task 8: Remove `ConfigPatch` async_mode references

**Files:**
- Modify: `src/config.rs` (or wherever `ConfigPatch` is defined)

- [ ] **Step 1: Search for `async_mode` in ConfigPatch and config code**

Run: `grep -rn "async_mode" src/ --include="*.rs"` to find remaining references.

- [ ] **Step 2: Remove async_mode from config structures**

Remove any `async_mode` fields from `ConfigPatch`, `FileConfig`, and related serialization/deserialization. Remove the `GIT_AI_ASYNC_MODE` env var handling anywhere it appears.

- [ ] **Step 3: Compile check**

Run: `cargo check --tests 2>&1 | head -80`

- [ ] **Step 4: Commit**

```bash
git add src/
git commit -m "refactor: remove async_mode from config structures"
```

---

### Task 9: Update CI and Taskfile

**Files:**
- Modify: `.github/workflows/test.yml`
- Modify: `Taskfile.yml`
- Modify: `scripts/ci-test-with-retry.sh`
- Modify: `scripts/ci-test-with-retry.ps1`

- [ ] **Step 1: Remove wrapper test mode from CI matrix**

In `.github/workflows/test.yml`, remove the three `test_mode: wrapper` matrix entries (Ubuntu lines 39-42, Windows lines 52-54, macOS lines 63-66).

- [ ] **Step 2: Remove deprecated Taskfile entries**

In `Taskfile.yml`, remove:
- `test:wrapper` task (lines 72-77)
- `test:bats` task (lines 79-83)

- [ ] **Step 3: Update CI retry scripts**

In `scripts/ci-test-with-retry.sh`, remove the wrapper mode check (lines 31-35). Since all modes are now daemon or wrapper-daemon, retry is always enabled. Simplify the conditional.

Similarly update `scripts/ci-test-with-retry.ps1`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/test.yml Taskfile.yml scripts/
git commit -m "ci: remove wrapper mode from test matrix and task runner"
```

---

### Task 10: Full build, lint, and test

- [ ] **Step 1: Run lint**

Run: `task lint`

Fix any warnings/errors.

- [ ] **Step 2: Run fmt**

Run: `task fmt`

- [ ] **Step 3: Run full test suite**

Run: `task test`

- [ ] **Step 4: Run wrapper-daemon tests**

Run: `task test:wrapper-daemon`

- [ ] **Step 5: Fix any failures and commit**

```bash
git add -A
git commit -m "fix: resolve lint/test issues from wrapper mode removal"
```

- [ ] **Step 6: Review snapshot changes if any**

Run: `cargo insta review` if snapshot tests need updating.
