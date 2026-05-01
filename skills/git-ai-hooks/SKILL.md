---
name: git-ai-hooks
description: "Understand or modify the git-ai hook system. Use when working on how git-ai intercepts git commands (commit, rebase, cherry-pick, stash, reset, checkout, push, fetch, etc.), how attribution is preserved through history-rewriting operations, how the rewrite log works, or how the signal forwarding and subprocess execution work."
argument-hint: "[hook or operation you want to understand or modify]"
allowed-tools: ["Read", "Grep", "Glob", "Bash", "Edit", "Write"]
---

# git-ai Hook Architecture

## Overview

When git-ai acts as a transparent git proxy (symlinked as `git`), it intercepts every git command and runs pre/post hooks around the real git invocation. This lets it track attribution through all history-rewriting operations.

```
git <command> args
    → git-ai binary (via symlink)
        → run_pre_command_hooks()  [panic-isolated]
        → proxy_to_git()           [real git]
        → run_post_command_hooks() [panic-isolated]
```

Every hook is wrapped in `std::panic::catch_unwind(AssertUnwindSafe(...))` — a panicking hook logs to observability and is silently swallowed. It never blocks the user's git command.

## Hook Modules

All hooks live in `src/commands/hooks/`. Each implements a pre/post pair around a git subcommand.

### `commit_hooks.rs`

**Pre**: Captures HEAD before commit (`require_pre_command_head()`), runs `pre_commit::pre_commit` (materializes virtual attributions for staged files). Skips on dry-run or bare repo.

**Post**: Skips on dry-run / pre-hook failure / failed exit. Detects `--amend` (emits `RewriteLogEvent::CommitAmend`) vs normal commit (`Commit`). Calls `Repository::handle_rewrite_log_event` → `rewrite_authorship_if_needed`. Suppresses output on `--porcelain/--quiet/-q/--no-status`.

### `rebase_hooks.rs`

**Pre**: Probes `.git/rebase-merge`/`.git/rebase-apply`. Detects if a rebase is already in progress (no double-start). Resolves `original_head` (positional branch arg) and `onto` (`--onto` flag or `@{upstream}`). Writes `RebaseStart` event with `is_interactive`.

**Post**: Bails if rebase still in progress (conflict pause → user runs `rebase --continue`). On failure writes `RebaseAbort`. On success calls `process_completed_rebase` → `build_rebase_commit_mappings` → emits `RebaseComplete` → triggers `rewrite_authorship_after_rebase_v2`.

**Commit mapping** (`build_rebase_commit_mappings`):
1. `merge_base(original_head, new_head)` to find split point
2. Walk `original_head..base` for original commits (git rev-list --ancestry-path)
3. Walk `new_head` first-parent for new commits, count-capped at original count

### `cherry_pick_hooks.rs`

**Pre**: Probes `CHERRY_PICK_HEAD`/`sequencer/`. Parses commit refs/ranges from CLI (handling `-m`, ranges via `git rev-list --reverse`, keywords `continue/abort/skip`). Writes `CherryPickStart`.

**Post**: Builds new-commit list via `walk_commits_to_base(repo, new_head, original_head)`. Emits `CherryPickComplete { source_commits, new_commits }` → `rewrite_authorship_after_cherry_pick`.

### `reset_hooks.rs`

**Pre**: Takes a `Human` checkpoint (captures working-tree state before reset), stores `pre_command_base_commit`, resolves tree-ish to SHA *before* reset runs (critical: `HEAD~1` resolves differently after reset).

**Post**: Branches on `--hard` / `--soft|--mixed|--merge` and presence of pathspecs:
- Hard reset: deletes working log for old HEAD
- Soft/mixed: `reconstruct_working_log_after_reset` rebuilds attributions when resetting backward; `apply_wrapper_plumbing_rewrite_if_possible` for non-ancestor resets (Graphite-style)
- Pathspec resets: merge non-pathspec checkpoints from old log with freshly-reconstructed pathspec-only log
- Appends `Reset` event

### `stash_hooks.rs`

**Pre**: For `pop`/`apply`/`branch` → resolve and store stash SHA *before* git deletes it. For create paths → run `Human` checkpoint.

**Post**:
- `push/save`: build `VirtualAttributions`, serialize as `AuthorshipLog`, save under `refs/notes/ai-stash` keyed by stash SHA
- `pop/apply/branch`: read stash note, convert attestations back to `LineAttribution` records, seed new working log via `write_initial_attributions_with_contents`
- Tolerates exit code 1 when `git status --porcelain=v2` shows unmerged entries

### `merge_hooks.rs`

**Post**: Only acts on `--squash` + success + non-dry-run. Captures source branch HEAD, base branch HEAD, and staged file blob OIDs → stores `MergeSquashEvent` so a later `git commit` can replay AI authorship from the squashed range.

### `checkout_hooks.rs` / `switch_hooks.rs`

**Pre**: Stores pre-command HEAD. On `--merge/-m`, captures `VirtualAttributions::from_just_working_log` into `stashed_va`.

**Post** (5 cases):
- Pathspec checkout: remove attributions for those paths
- HEAD unchanged: no-op
- `-f/--force`: delete working log
- `--merge`: restore stashed VA via `restore_stashed_va` (re-resolves line numbers in new working tree); skip if conflict markers present (byte offsets would be wrong)
- Otherwise: rename working log from old to new HEAD

`switch` mirrors checkout but adds `--discard-changes` to the force-flag set.

### `fetch_hooks.rs`

**Pre** (fetch): Spawns a background thread that calls `fetch_authorship_notes` in parallel with the main fetch. Returns `JoinHandle` for the post-hook to join.

**Pre** (pull): Layered on fetch pre-hook. For `pull --rebase`, writes a *speculative* `RebaseStart` (so `rebase --continue` after a conflict has a recoverable original head). For `pull --rebase --autostash` with dirty tree, captures `VirtualAttributions` into `stashed_va`.

**Post** (pull): Joins fetch thread. Restores stashed VA when HEAD moved. Calls `rename_working_log` for fast-forward pulls (detected from reflog: `pull: Fast-forward`). Runs `process_completed_pull_rebase`. Cancels speculative `RebaseStart` with `RebaseAbort` when no conflict occurred.

### `push_hooks.rs`

**Pre**: Spawns background thread for `push_authorship_notes` to resolved remote. Skips on `--dry-run`, `-d/--delete`, `--mirror`.

**Post**: Joins push thread (regardless of main push success).

### `clone_hooks.rs`

**Post**: Extracts target dir, opens new repo, checks `Config::is_allowed_repository`, fetches `refs/notes/ai*` from origin via `sync_authorship::fetch_authorship_notes`.

### `update_ref_hooks.rs`

**Pre**: Parses non-stdin, non-deletion `update-ref <name> <new> [<old>]`. Captures old target SHA and whether the ref is HEAD or matches the current branch.

**Post**: If ref moved forward → rename working log when it's the checked-out branch. For non-ancestor rewrites (Graphite restack) → `apply_wrapper_plumbing_rewrite_if_possible` synthesizes a `RebaseCompleteEvent`.

## CommandHooksContext — Shared State

Pre-hooks store state here; post-hooks consume it. Key fields:

```rust
struct CommandHooksContext {
    pre_commit_hook_result: Option<bool>,
    rebase_original_head: Option<String>,
    rebase_onto: Option<String>,
    fetch_authorship_handle: Option<JoinHandle<()>>,
    push_authorship_handle: Option<JoinHandle<()>>,
    stash_sha: Option<String>,
    stashed_va: Option<VirtualAttributions>,
}
```

## Rewrite Log

`src/git/rewrite_log.rs` — `.git/ai/rewrite_log` (JSONL, **newest-first**)

Records every history-rewriting operation so daemon mode and cross-process recovery can replay state:

```
RewriteLogEvent variants:
  Commit { base_commit, commit_sha }
  CommitAmend { original_commit, amended_commit_sha }
  MergeSquash { source_branch, source_head, base_branch, base_head, staged_file_blobs }
  RebaseStart { original_head, is_interactive, onto_head }
  RebaseComplete { original_head, new_head, is_interactive, original_commits, new_commits }
  RebaseAbort { original_head }
  CherryPickStart/Complete/Abort (parallel structure)
  Reset { kind: Hard|Soft|Mixed, new_head_sha, old_head_sha, ... }
  Stash { operation: Create|Apply|Pop|Drop|... }
  AuthorshipLogsSynced { synced, origin, timestamp }
```

**Format**: `#[serde(untagged)]` — each variant identified by its top-level key name. Malformed lines silently skipped. Maximum 200 events.

**Append logic**: reads existing → parse → prepend new event → deduplicate → truncate to 200 → rewrite whole file.

**Recovery pattern**: walk events newest-first, short-circuit on terminal events (e.g. `RebaseComplete` or `RebaseAbort` found before the first `RebaseStart` → `has_active_rebase_start_event = false`).

## Rebase Authorship Rewrite Algorithm

`src/authorship/rebase_authorship.rs` — entry: `rewrite_authorship_if_needed`

### Fast Path (no-op rebase)

If rebased commits' tracked file contents match the originals: just rewrite each note's `base_commit_sha` field. No blame needed.

### Slow Path (`rewrite_authorship_after_rebase_v2`)

1. Single combined `git diff-tree -p --stdin` call for both sides (new commits + original commits) — one subprocess for all diffs
2. Partition into `new_commit_deltas` and `original_hunks_by_commit`
3. Reconstruct attribution baseline at `original_head` (from cached notes, or blame fallback)
4. For each `(original, new)` commit pair in topological order:
   - Apply new commit's hunks to running line-attribution map
   - Transfer original commit's attribution where hunks match; diff-based transfer otherwise
5. Write fresh `AuthorshipLog` notes for each new commit
6. `migrate_working_log_after_rebase(original_head, new_head)` — rename or merge the working log directory

### Working Log Keying Through Rewrites

| Operation | Working log change |
|---|---|
| Plain commit | Consume `<parent>/`, write INITIAL to `<new_commit>/` |
| Amend | Rename `<orig>/` → `<amended>/` |
| Rebase | Rename `<original_head>/` → `<new_head>/`, or merge INITIAL when both exist |
| `reset --hard` | Delete `<old_head>/` |
| `reset --soft/--mixed` | `reconstruct_working_log_after_reset` → write INITIAL to `<target>/` |
| Stash push | Serialize working log to note under `refs/notes/ai-stash`, clear from INITIAL |
| Stash pop | Read stash note, write INITIAL to `<HEAD>/` |
| Checkout (force/branch) | Rename working log directory |

**Invariant**: `<base_commit>` in the working log path always matches current HEAD by end of a successful hook cycle.

## Git Subprocess Execution

`src/git/repository.rs`

```rust
repo.exec_git(args)                   // errors on non-zero exit
repo.exec_git_allow_nonzero(args)     // returns Output; caller checks status
repo.exec_git_stdin(args, data)       // pipes data to stdin on separate thread (avoids deadlock)
repo.exec_git_stdin_with_env(args, env, data)
```

Cross-cutting behaviors in every variant:
- Injects `-c core.hooksPath=/dev/null` when `should_disable_internal_git_hooks()` is true (prevents recursion when git-ai calls git internally)
- Removes `GIT_EXTERNAL_DIFF` and `GIT_DIFF_OPTS` to prevent user diff drivers from corrupting parsed output
- On Windows: `CREATE_NO_WINDOW` when stdin is not a TTY

**Stdin deadlock prevention**: `exec_git_stdin` spawns a separate thread to write stdin while the main thread calls `wait_with_output()`. `BrokenPipe` from the writer is tolerated.

## Command Classification

`src/git/command_classification.rs`

`is_definitely_read_only_invocation(command, subcommand)` — returns true for `blame`, `log`, `status`, `diff`, `diff-index`, `rev-list`, `grep`, `stash list`, `stash show`, `worktree list`, etc. Anything not listed is treated as mutating.

Read-only invocations bypass the entire wrapper hook chain and suppress trace2 events — critical for IDE git panels (Zed, VS Code) that call git thousands of times per session.

## Signal Forwarding (Unix)

Child processes are placed in their own process group (`setpgid(0, 0)` via `pre_exec`). SIGTERM/SIGINT/SIGHUP/SIGQUIT are forwarded to the entire group (`kill(-pgid, sig)`).

Skip when stdin is a TTY — child must inherit the foreground terminal pgrp for interactive flows, otherwise SIGTTIN/SIGTTOU would suspend it.

On exit: if child died by signal → re-raise that signal after restoring `SIG_DFL` so the wrapper's exit status matches the child's.

## Adding or Modifying a Hook

1. Hook file: `src/commands/hooks/<subcommand>_hooks.rs`
2. Register pre/post functions in `src/commands/hooks/mod.rs`
3. Wire into dispatch in `src/commands/git_handlers.rs` `run_pre_command_hooks` / `run_post_command_hooks`
4. If state must flow pre→post: add a field to `CommandHooksContext`
5. If state must survive across processes (daemon mode): add a `RewriteLogEvent` variant and write it in the pre-hook; read it back in post-hook recovery paths
6. If the hook rewrites history: update `migrate_working_log_after_rebase` or add a new working-log migration path

## Relevant Files

| File | Purpose |
|---|---|
| `src/commands/git_handlers.rs` | Dispatcher, proxy_to_git, signal forwarding, CommandHooksContext |
| `src/commands/hooks/mod.rs` | Hook module exports |
| `src/commands/hooks/commit_hooks.rs` | Commit pre/post |
| `src/commands/hooks/rebase_hooks.rs` | Rebase pre/post, commit mapping |
| `src/commands/hooks/cherry_pick_hooks.rs` | Cherry-pick pre/post |
| `src/commands/hooks/reset_hooks.rs` | Reset pre/post, working log reconstruction |
| `src/commands/hooks/stash_hooks.rs` | Stash pre/post, stash note serialize/restore |
| `src/commands/hooks/merge_hooks.rs` | Squash merge event recording |
| `src/commands/hooks/checkout_hooks.rs` | Checkout pre/post, stashed VA restore |
| `src/commands/hooks/fetch_hooks.rs` | Fetch/pull pre/post, background note sync |
| `src/commands/hooks/push_hooks.rs` | Push pre/post, background note push |
| `src/commands/hooks/clone_hooks.rs` | Clone post, fetch initial notes |
| `src/commands/hooks/update_ref_hooks.rs` | update-ref pre/post |
| `src/authorship/rebase_authorship.rs` | Rebase/cherry-pick authorship rewrite (267KB) |
| `src/git/rewrite_log.rs` | RewriteLogEvent schema, JSONL append |
| `src/git/repository.rs` | exec_git*, find_repository |
| `src/git/repo_state.rs` | CLI-free worktree/HEAD/reflog parsing |
| `src/git/command_classification.rs` | Read-only command allowlist |
