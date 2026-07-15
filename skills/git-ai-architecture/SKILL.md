---
name: git-ai-architecture
description: "Understand or navigate the git-ai codebase architecture. Use when you need to know how the binary dispatch works, where to add new commands, how error handling works, how feature flags are managed, how the config singleton works, or how cross-platform concerns are handled."
argument-hint: "[architectural question or area to understand]"
allowed-tools: ["Read", "Grep", "Glob", "Bash"]
---

# git-ai Architecture

## Binary Dispatch — How One Binary Does Two Jobs

`src/main.rs` dispatches based on `argv[0]`:

```
argv[0] == "git-ai"  →  commands::git_ai_handlers::handle_git_ai()  (direct subcommands)
argv[0] == "git"     →  commands::git_handlers::handle_git()          (transparent proxy)
```

In production, the binary is symlinked as `git` so it intercepts all git commands. In debug builds, setting `GIT_AI=git` forces proxy mode regardless of binary name — this is how integration tests invoke the binary as a git proxy without symlinking.

```rust
// debug-only shortcut in main.rs:
#[cfg(debug_assertions)]
if std::env::var("GIT_AI").as_deref() == Ok("git") {
    commands::git_handlers::handle_git(&cli.args);
    return;
}
```

Clap is configured with `disable_help_flag = true`, `disable_version_flag = true`, `trailing_var_arg = true`, `allow_hyphen_values = true` — it does trivial argv collection because the binary cannot consume any flags meant for git.

## Git Proxy Flow (handle_git)

`src/commands/git_handlers.rs`

1. Parse argv → `ParsedGitInvocation`
2. Resolve git aliases via `resolve_alias_invocation` (handles cycles, shell aliases)
3. `run_pre_command_hooks()` — wrapped in `std::panic::catch_unwind` per hook so panics never abort the user's git command
4. `proxy_to_git()` — spawn the real git binary
5. `run_post_command_hooks()` — also panic-caught

A `CommandHooksContext` struct threads state from pre to post hooks (rebase original head, stash SHA, async join handles, stashed VirtualAttributions).

**Read-only commands short-circuit entirely**: `command_classification::is_definitely_read_only_invocation` bypasses the wrapper for `blame`, `log`, `status`, `diff`, `stash list`, `worktree list`, etc. — critical for performance with IDE git panels (thousands of calls/session).

**Daemon mode** (`feature_flags().async_mode`): wrapper becomes a pure git passthrough + daemon ping. Pre/post state is sent to the daemon which handles attribution asynchronously.

## Direct Subcommands (handle_git_ai)

`src/commands/git_ai_handlers.rs`

A large `match args[0].as_str()` over two dozen subcommands: `checkpoint`, `stats`, `status`, `show`, `blame`, `diff`, `log`, `config`, `bg`/`d`/`daemon`, `install-hooks`, `ci`, `upgrade`, `login`/`logout`/`whoami`, `dashboard`, `share`, `sync-prompts`, `prompts`, `search`, `continue`, `fetch-notes`, etc.

Notable patterns:
- `InternalDatabase::warmup()` is called early for DB-heavy commands (`checkpoint`, `show-prompt`, `share`, `search`, `continue`)
- `is_interactive_terminal()` gates observability event emission (only from real TTYs)
- Debug-only commands are wrapped in `#[cfg(debug_assertions)]`
- Version output: `cfg!(debug_assertions)` appends `(debug)` suffix

## Error Type Design

`src/error.rs` — hand-rolled (no `thiserror`):

```rust
pub enum GitAiError {
    #[cfg(feature = "test-support")]
    GitError(git2::Error),           // test-only: libgit2 errors
    IoError(std::io::Error),
    GitCliError {                    // structured: code + stderr + original args
        code: Option<i32>,
        stderr: String,
        args: Vec<String>,
    },
    GixError(String),
    JsonError(serde_json::Error),
    Utf8Error(std::str::Utf8Error),
    FromUtf8Error(std::string::FromUtf8Error),
    PresetError(String),             // user-facing, no prefix in Display
    SqliteError(rusqlite::Error),
    Generic(String),
}
```

Key design choices:
- `GitCliError` is **structured** — always shows the invocation + exit code + stderr to the user
- `git2::Error` is gated behind `#[cfg(feature = "test-support")]` — production never depends on libgit2
- Manual `Clone` impl flattens non-Clone inner types into `Generic(format!(...))` preserving the message
- `PresetError` displays bare (no prefix) because it's already prose-shaped for the user
- `From<T>` impls for `io::Error`, `serde_json::Error`, `Utf8Error`, `FromUtf8Error`, `rusqlite::Error`

## Feature Flag System

`src/feature_flags.rs` — the `define_feature_flags!` macro generates three things:

```rust
define_feature_flags!(
    rewrite_stash:    rewrite_stash,              debug = true,  release = true,
    inter_commit_move: checkpoint_inter_commit_move, debug = false, release = false,
    auth_keyring:     auth_keyring,               debug = false, release = false,
    async_mode:       async_mode,                 debug = false, release = true,  // ← diverges!
);
```

Two-name design: `$field` = Rust struct field; `$file_name` = JSON config key AND env var suffix (`GIT_AI_<FILE_NAME>`).

**Precedence** (highest to lowest):
1. Env vars (`GIT_AI_*` prefix, via `envy`)
2. `~/.git-ai/config.json` file
3. Compiled-in debug/release defaults

`async_mode` is the canonical example of debug/release divergence: false in debug (tests run wrapper mode by default), true in release (daemon mode for production). Tests respect `GIT_AI_TEST_GIT_MODE` env var to override.

## Config Singleton

`Config::get()` — global `OnceLock` accessed everywhere, reads from `~/.git-ai/config.json`.

Test isolation:
- `GIT_AI_TEST_CONFIG_PATCH` env var carries a `ConfigPatch` JSON blob that overrides specific fields without writing to disk
- `GIT_AI_TEST_DB_PATH` places SQLite outside `.git/` (as a sibling) to prevent WAL/SHM interference with git operations

## Logging Conventions — Three Layers

| Layer | API | When |
|---|---|---|
| User-facing debug | `debug_log!(...)` | User-visible `[git-ai]` prefixed stderr messages; active when `cfg!(debug_assertions)` or `GIT_AI_DEBUG=1`; silenced by `GIT_AI_DEBUG=0` |
| Internal tracing | `tracing::debug!(...)` | Subsystem diagnostics (hook skip reasons, daemon plumbing) |
| Observability telemetry | `observability::log_error(...)` / `log_message(...)` | Structured events to daemon or per-PID log files; gated by `is_interactive_terminal()` |

Performance logging: `GIT_AI_DEBUG_PERFORMANCE=1` (human-readable) or `=2` (JSON) emits per-phase durations for `pre_command`, `git`, `post_command`.

## Cross-Platform Patterns

**Signal forwarding (Unix only)** — `git_handlers.rs`:

```rust
#[cfg(unix)]
static CHILD_PGID: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn forward_signal_handler(sig: libc::c_int) {
    let pgid = CHILD_PGID.load(Ordering::Relaxed);
    if pgid > 0 { unsafe { let _ = libc::kill(-pgid, sig); } }
}
```

Child is placed in its own process group (`setpgid(0, 0)` via `pre_exec`) so SIGTERM/SIGINT/SIGHUP/SIGQUIT forward to the whole group. Skip when stdin is a TTY (avoids SIGTTIN/SIGTTOU suspension).

On exit, if child died by signal → re-raise the same signal after restoring `SIG_DFL` so callers see authentic signal-death exit status.

**Windows specifics:**
- `CREATE_NO_WINDOW` flag when stdin is not a TTY (no console popup)
- NTSTATUS `0xC000013A` detection for Ctrl-C exit
- `NUL` instead of `/dev/null` for null hooks path
- Platform-gated deps: `named_pipe`, `winreg`

**Import gating:**
```rust
#[cfg(unix)] use std::os::unix::process::CommandExt;
#[cfg(windows)] use std::os::windows::process::CommandExt;
```

## Rust Edition / Version Details

- `edition = "2024"`, `rust-toolchain = "1.93.0"`
- Stable let-chains (`if let Some(x) = foo && condition { ... }`) used throughout
- `unsafe { std::env::set_var/remove_var }` — required in edition 2024
- `OnceLock` for global singletons
- `test-support` Cargo feature gates `git2` dep and exposes internal functions for testing:

```rust
#[cfg(feature = "test-support")] pub fn internal_fn() { ... }
#[cfg(not(feature = "test-support"))] fn internal_fn() { ... }
```

## Library vs Binary

`src/lib.rs` exports all modules as `pub mod` (api, auth, authorship, ci, commands, config, daemon, error, feature_flags, git, http, mdm, metrics, observability, repo_url, utils). Everything is reachable from integration tests in `tests/`. Finer-grained visibility uses `pub(crate)`.

One integration test target:
```toml
[[test]]
name = "integration"
path = "tests/integration/main.rs"
harness = true
```

## Panic Isolation Around Hooks

Every hook call in `run_pre_command_hooks` and `run_post_command_hooks` is wrapped:

```rust
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    // hook call
}));
if let Err(panic_payload) = result {
    // extract message, log to observability, never abort the user's git command
}
```

## File Navigation Guide

| What you're looking for | Where to look |
|---|---|
| Binary entry point, dispatch | `src/main.rs` |
| Git proxy hooks dispatch | `src/commands/git_handlers.rs` |
| Direct subcommand dispatch | `src/commands/git_ai_handlers.rs` |
| Error types | `src/error.rs` |
| Feature flags macro | `src/feature_flags.rs` |
| Config struct | `src/config.rs` (or `src/commands/config.rs`) |
| Hook implementations | `src/commands/hooks/` |
| Attribution engine | `src/authorship/` |
| Checkpoint command | `src/commands/checkpoint.rs` |
| Agent presets | `src/commands/checkpoint_agent/` |
| Git subprocess execution | `src/git/repository.rs` |
| Working log storage | `src/authorship/working_log.rs` |
| Rebase/rewrite authorship | `src/authorship/rebase_authorship.rs` |
| CI integration | `src/ci/` + `src/commands/ci_handlers.rs` |
| MDM agent detection | `src/mdm/agents/` |
| Test harness | `tests/integration/repos/` |
