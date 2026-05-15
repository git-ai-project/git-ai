# Commands

CLI entry points for both `git-ai` subcommands and git hook handlers.

## Subcommands (`git-ai <cmd>`)

| File | Command | Purpose |
|------|---------|---------|
| `checkpoint.rs` | `git-ai checkpoint <agent>` | Record AI/human file edits from agent hooks |
| `blame.rs` | `git-ai blame` | `git blame` annotated with AI/human attribution |
| `diff.rs` | `git-ai diff` | `git diff` annotated with attribution |
| `status.rs` | `git-ai status` | Show attribution stats for working tree |
| `install.rs` | `git-ai install` | Install agent hooks and daemon |
| `fetch_notes.rs` | `git-ai fetch-notes` | Fetch `refs/notes/ai` from remote |
| `bg.rs` | `git-ai bg` | Daemon lifecycle (start/stop/restart/status) |
| `ci.rs` | `git-ai ci` | CI attribution reporting |
| `internal.rs` | `git-ai internal` | Internal/debugging commands |

## Git hook handlers

| File | Hook | Purpose |
|------|------|---------|
| `post_commit.rs` | post-commit | Generate authorship note from working log |
| `post_rewrite.rs` | post-rewrite | Rewrite authorship notes after rebase/amend |
| `stash.rs` | pre/post stash | Preserve working log across stash operations |

## How dispatch works

`main.rs` routes to this module based on `argv[0]`:
- `argv[0] == "git-ai"` → subcommand dispatch
- Daemon events → the daemon calls into `post_commit` and `post_rewrite` directly

## Helpers

`helpers.rs` provides shared utilities: repository discovery, note reading/writing, git command execution.
