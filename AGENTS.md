# CLAUDE.md -- git-ai project guide

## Build & Test Commands

```bash
# Build
cargo build                              # debug build (used by integration tests)
cargo build --release                    # release build
cargo build --features test-support      # debug build with git2 (needed for test binary)

# Test (integration tests auto-compile a test-support debug binary via OnceLock)
cargo test                               # all tests (parallel)
cargo test -- --test-threads=8           # CI thread count
cargo test <test_name>                   # single test by function name
cargo test --test simple_additions       # single test file (tests/simple_additions.rs)
cargo test --test rebase                 # another test file example
cargo test -- --ignored                  # run #[ignore]'d e2e/SCM tests

# Lint & Format
# CI uses Rust 1.93.0 pinned, RUSTFLAGS="-D warnings" (warnings are errors)
cargo clippy                             # lint (CI runs on all 3 platforms)
cargo fmt -- --check                     # format check
cargo fmt                                # auto-format

# E2E tests (requires bats shell testing framework + debug build)
bats tests/e2e/user-scenarios.bats

# Snapshot management (insta crate)
cargo insta review                       # interactively review snapshot changes
cargo insta accept                       # accept all pending snapshots
```

**Dev environment**: Use `nix develop` to get the pinned Rust 1.93.0 toolchain and dev wrappers (`git`, `git-ai`, `git-og`). The shell hook creates wrapper scripts in `~/.git-ai-local-dev/gitwrap/bin/` that point to `target/debug/git-ai`. Use `git-og` to bypass git-ai and call real git.

## Architecture

### Binary dispatch (src/main.rs)

A single binary serves two roles based on `argv[0]`:
- **`argv[0] == "git"`** --> `commands::git_handlers::handle_git()` -- proxies to real git with pre/post hooks per subcommand
- **`argv[0] == "git-ai"`** --> `commands::git_ai_handlers::handle_git_ai()` -- direct subcommands (checkpoint, blame, diff, status, search, etc.)
- **Debug-only shortcut**: When `cfg!(debug_assertions)` and `GIT_AI=git` env var is set, forces git proxy mode regardless of binary name. This is how integration tests invoke the binary as a git proxy without symlinking.

### Core data flow: checkpoint --> working log --> authorship note

1. **Checkpoint**: An AI coding agent calls `git-ai checkpoint <agent>` with hook input (JSON on stdin or env var). The agent preset (`src/commands/checkpoint_agent/agent_presets.rs`) extracts edited file paths, transcript, and model info. The checkpoint processor diffs the working tree against HEAD to compute character-level attributions.

2. **Working log**: Checkpoint data is written to `.git/ai/working_logs/<base_commit>/` as JSON files. Each working log entry records per-file line attributions (which ranges are AI vs human) and prompt metadata.

3. **Post-commit hook**: On `git commit`, the post-commit hook reads working logs, generates an `AuthorshipLog` (schema version `authorship/3.0.0`), and stores it as a Git Note under `refs/notes/ai`. The authorship log contains attestation entries (hash --> line ranges) and a metadata section with prompt records.

4. **Rewrite tracking**: The `rewrite_log` (`.git/ai/rewrite_log`) records history-rewriting git operations (rebase, cherry-pick, reset, merge, stash, amend). Post-hooks for these commands use `rebase_authorship.rs` to rewrite authorship notes so attribution follows code through history rewrites.

### Git proxy hook architecture (src/commands/hooks/)

Each git subcommand has dedicated pre/post hooks:
- `commit_hooks` -- pre: captures virtual attributions; post: generates authorship note
- `rebase_hooks` -- pre: records original HEAD/onto; post: rewrites authorship notes for rebased commits
- `cherry_pick_hooks` -- post: copies/adapts authorship from source commit
- `reset_hooks` -- post: reconstructs working logs when commits are un-done
- `stash_hooks` -- preserves uncommitted AI attributions across stash/pop
- `merge_hooks`, `checkout_hooks`, `switch_hooks`, `fetch_hooks`, `push_hooks`, `clone_hooks`

Signal forwarding: On Unix, the git proxy installs signal handlers (SIGTERM, SIGINT, SIGHUP, SIGQUIT) that forward to the child git process group.

### Config singleton

`Config` is a global `OnceLock` singleton accessed via `Config::get()`. It reads from `~/.git-ai/config.json`. In tests, `GIT_AI_TEST_CONFIG_PATCH` env var allows overriding specific config fields without a real config file. Feature flags follow precedence: environment vars (`GIT_AI_*` prefix via `envy`) > config file > defaults.

Feature flags have separate debug/release defaults defined via the `define_feature_flags!` macro in `src/feature_flags.rs`. Currently: `rewrite_stash` (debug=true, release=false), `inter_commit_move` (false/false), `auth_keyring` (false/false).

### Error handling

`GitAiError` enum in `src/error.rs` -- not `thiserror`-based, uses manual `Display`/`From` impls. Variants: `GitCliError` (captures exit code + stderr + args), `IoError`, `JsonError`, `SqliteError`, `PresetError`, `Generic`, `GixError`. The `GitError(git2::Error)` variant only exists behind `#[cfg(feature = "test-support")]`.

## Test Infrastructure

### Integration test framework (tests/repos/)

Tests create real git repositories using `git2` crate (behind `test-support` feature). The test framework has three key files:

- **`tests/repos/test_repo.rs`** -- `TestRepo` struct: creates temp git repos, runs git-ai commands as subprocess. Uses `get_binary_path()` which auto-compiles the binary with `--features test-support` via a `OnceLock`. Tests invoke the binary with `GIT_AI=git` env var to trigger git proxy mode.

- **`tests/repos/test_file.rs`** -- `TestFile` fluent API for setting file contents with attribution expectations. The `lines!` macro + `.ai()` / `.human()` trait methods create `ExpectedLine` vectors. `assert_lines_and_blame()` validates both content and AI/human attribution.

- **`tests/repos/mod.rs`** -- `subdir_test_variants!` macro auto-generates two test variants: one from a subdirectory and one using `-C` flag, to verify repository discovery works from any CWD.

Test pattern:
```rust
#[test]
fn test_example() {
    let repo = TestRepo::new();
    let mut file = repo.filename("test.txt");
    file.set_contents(lines!["Line 1", "AI line".ai()]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    file.assert_lines_and_blame(lines!["Line 1".human(), "AI line".ai()]);
}
```

### Test isolation

- Each `TestRepo` gets a random temp directory and a separate `GIT_AI_TEST_DB_PATH` (SQLite DB placed as sibling to repo, not inside, to avoid git conflicts with WAL files).
- `GIT_AI_TEST_CONFIG_PATCH` env var passes `ConfigPatch` JSON to override config in subprocess.
- Background flush is skipped when `GIT_AI_TEST_DB_PATH` is set (prevents race conditions on temp dir cleanup).
- Use `#[serial_test::serial]` for tests that conflict on shared env vars.

### Snapshot tests

Uses `insta` crate. Snapshots live in `tests/snapshots/` and `tests/repos/snapshots/`. Run `cargo insta review` to update.

## Key Conventions

- **Rust 2024 edition** with Rust 1.93.0 -- uses let-chains (`if let Some(x) = foo && condition`), which are stable in edition 2024.
- **Git CLI over libgit2 in production**: All git operations use `std::process::Command` to call the real git binary. The `git2` crate is test-only (`test-support` feature). This is intentional -- the binary acts as a transparent git proxy.
- **`debug_log()`** for conditional debug output: prints `[git-ai]` prefixed messages to stderr when `cfg!(debug_assertions)` or `GIT_AI_DEBUG=1`. Set `GIT_AI_DEBUG=0` to suppress in debug builds.
- **`GIT_AI_DEBUG_PERFORMANCE=1`** (or `=2` for JSON) enables performance timing output.
- **Paths are POSIX-normalized**: `normalize_to_posix()` utility converts Windows backslashes. File paths in authorship logs and working logs always use forward slashes.
- **`GIT_AI_VERSION` constant** changes between debug/release/test modes via `cfg` attributes in `authorship_log_serialization.rs`.
- **Cross-platform**: `#[cfg(unix)]` / `#[cfg(windows)]` conditional compilation is used throughout for signal handling, process creation flags (`CREATE_NO_WINDOW`), path handling, and terminal detection. 63 `#[cfg(windows)]` annotations exist across 17 files.

## Gotchas

- **Test binary auto-compilation**: Integration tests trigger `cargo build --bin git-ai --features test-support` on first test run via `OnceLock`. If you change code and run tests, the test harness recompiles. This can cause confusion if you're debugging -- the test binary is always a debug build at `target/debug/git-ai`.

- **argv[0] dispatch is load-bearing**: The binary's behavior is entirely determined by how it's invoked. In production, symlinking as `git` makes it a proxy. In tests, `GIT_AI=git` env var forces proxy mode (debug builds only). Breaking this dispatch breaks everything.

- **Config is process-global**: `Config` uses `OnceLock`, so it's initialized once per process and cannot be changed. Tests run git-ai as a subprocess and pass config overrides via `GIT_AI_TEST_CONFIG_PATCH` env var. You cannot change config mid-test within the same process.

- **Feature flag debug/release divergence**: `rewrite_stash` defaults to true in debug but false in release. Tests run debug builds, so they exercise stash rewriting by default. A test passing in debug may behave differently in release if it depends on this flag.

- **Working log base commit**: Working logs are keyed by the HEAD commit at checkpoint time (`.git/ai/working_logs/<sha>/`). If HEAD changes between checkpoint and commit (e.g., rebase), the post-commit hook must find and reconcile the correct working log.

- **Large source files**: Several core files exceed 50K-100K lines. `rebase_authorship.rs` (~119K), `agent_presets.rs` (~101K), `repository.rs` (~96K), `attribution_tracker.rs` (~87K). Navigate with grep, not scrolling.

- **Git notes namespace**: Authorship data lives in `refs/notes/ai`. Running `git notes` (default namespace) won't show it -- use `git notes --ref=ai list` or `git log --notes=ai`.

- **Snapshot tests can cascade**: Changing attribution logic can invalidate many snapshots at once. Use `cargo insta review` rather than manually editing `.snap` files.

- **Test parallelism**: Tests default to parallel execution. Most tests are isolated via temp directories, but tests using `serial_test::serial` exist where env var conflicts would cause flakiness. If adding tests that set process-global state, use `#[serial_test::serial]`.

- **SQLite WAL files**: Test DB paths are placed as siblings to the repo directory (not inside `.git/`) to prevent WAL/SHM files from interfering with git operations.

- **`smol` async runtime**: The project uses `smol` (not tokio) for async operations with `futures` combinators. The async surface area is small -- mostly HTTP operations and background flushes.
