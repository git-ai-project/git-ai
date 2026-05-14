# Manual Testing Guide

This guide walks through manually testing git-ai v2 with Cursor, VS Code, Claude Code, and the git CLI.

## Prerequisites

1. Build and install git-ai v2:
```bash
cd /path/to/git-ai-v2
cargo build --release
cp target/release/git-ai ~/.git-ai/bin/git-ai
```

2. Install in your test repo:
```bash
cd /path/to/test-repo
git-ai install
```

This does three things:
- Kills any running v1 daemon
- Installs a `post-commit` hook in `.git/hooks/`
- Configures `trace2.eventTarget` in global git config to point to the daemon socket

3. Start the daemon:
```bash
git-ai bg start
```

Verify it's running:
```bash
git-ai bg status
```

## Testing with Cursor

### Setup

Add to Cursor's settings (Settings > Features > Rules for AI > User Rules, or `.cursor/rules`):

Cursor dispatches hooks via its built-in hook system. The checkpoint command must be available on PATH or referenced by absolute path.

In Cursor's `~/.cursor/hooks/hooks.json` (or project-level `.cursor/hooks.json`):
```json
{
  "hooks": {
    "preToolUse": [
      {
        "command": "git-ai checkpoint cursor --hook-input stdin"
      }
    ],
    "postToolUse": [
      {
        "command": "git-ai checkpoint cursor --hook-input stdin"
      }
    ]
  }
}
```

### Test: single file edit

1. Open a test repo in Cursor
2. Ask the AI to edit a file (e.g., "add a hello world function to main.py")
3. Wait for the edit to complete
4. Commit the change: `git commit -am "test cursor edit"`
5. Verify attribution:
```bash
git-ai blame main.py
```

Expected: lines written by Cursor show the AI agent icon/label. Lines you typed show as human.

### Test: multi-file edit

1. Ask Cursor to refactor across multiple files
2. Commit all changes
3. Run `git-ai blame` on each modified file
4. Verify each file has correct AI attribution on the changed lines

### Test: shell tool

1. Ask Cursor to run a shell command that creates/modifies a file (e.g., "run `echo hello > output.txt`")
2. Commit the result
3. Run `git-ai blame output.txt`
4. Verify the lines are attributed to AI (shell tool edits are tracked)

### Test: mixed human + AI

1. Manually type a few lines in a file
2. Ask Cursor to add more lines to the same file
3. Commit
4. Run `git-ai blame` — human-typed lines should show as human, AI lines as AI

## Testing with VS Code

### Setup

Install the git-ai VS Code extension from the marketplace (or build from `agent-support/vscode/`):
```bash
cd agent-support/vscode
yarn install && yarn compile
# Install via "Extensions: Install from VSIX" or link for development
```

The VS Code extension handles:
- Detecting human keystrokes (known human checkpoints)
- AI tab completion tracking (experimental, enable via `gitai.experiments.aiTabTracking`)

### Test: human edit detection

1. Open a file in VS Code
2. Type several new lines manually
3. Commit
4. Run `git-ai blame` — your typed lines should show as "known human" (not untracked)

### Test: Copilot integration

If using GitHub Copilot in VS Code, configure the hook in Copilot's settings. The preset is `github-copilot`:

```json
{
  "hooks": {
    "before_edit": [
      {"command": "git-ai checkpoint github-copilot --hook-input stdin"}
    ],
    "after_edit": [
      {"command": "git-ai checkpoint github-copilot --hook-input stdin"}
    ]
  }
}
```

1. Accept a Copilot suggestion
2. Commit
3. `git-ai blame` should show the accepted lines as AI-authored

## Testing with Claude Code

### Setup

Add hooks to `~/.claude/settings.json`:
```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "/home/YOUR_USER/.git-ai/bin/git-ai checkpoint claude --hook-input stdin"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "/home/YOUR_USER/.git-ai/bin/git-ai checkpoint claude --hook-input stdin"
          }
        ]
      }
    ]
  }
}
```

Replace `/home/YOUR_USER/.git-ai/bin/git-ai` with the actual path to the binary.

### Test: file write

1. Ask Claude Code to create or edit a file
2. Commit the change
3. Verify:
```bash
git-ai blame <file>
```

### Test: bash tool

1. Ask Claude Code to run a command that modifies files
2. Commit
3. `git-ai blame` should attribute the changes to AI

### Test: multi-turn session

1. Have Claude Code make several edits across multiple turns
2. Commit once at the end
3. All AI-edited lines should be attributed, with the session ID grouping them

## Testing the Git CLI (no agent)

These tests verify core git-ai behavior independent of any AI agent.

### Test: daemon processes commits

1. Ensure daemon is running: `git-ai bg status`
2. Make a change and commit:
```bash
echo "test line" >> file.txt
git add file.txt
git commit -m "daemon test"
```
3. Check that the note was created:
```bash
git notes --ref=ai show HEAD
```
4. Check the marker file exists:
```bash
ls .git/ai/noted/$(git rev-parse HEAD)
```

### Test: post-commit fallback (no daemon)

1. Stop the daemon: `git-ai bg stop`
2. Make a change and commit
3. Verify the note still gets written:
```bash
git notes --ref=ai show HEAD
```
4. Restart daemon: `git-ai bg start`

### Test: blame output

```bash
# Basic blame
git-ai blame src/main.rs

# Blame specific lines
git-ai blame -L 10,20 src/main.rs

# JSON output
git-ai blame --json src/main.rs
```

### Test: diff output

```bash
# Show AI-attributed lines in the last commit
git-ai diff HEAD~1..HEAD

# Show uncommitted attribution status
git-ai status
```

### Test: stats

```bash
# Show attribution stats for recent commits
git-ai stats

# Stats for a specific range
git-ai stats HEAD~5..HEAD
```

### Test: rebase preserves attribution

1. Create a branch with AI-attributed commits
2. Rebase onto main:
```bash
git rebase main
```
3. Verify authorship notes were carried forward:
```bash
git notes --ref=ai list
git-ai blame <file>
```

### Test: cherry-pick preserves attribution

1. Cherry-pick a commit with AI attribution:
```bash
git cherry-pick <sha>
```
2. Verify the note was copied:
```bash
git notes --ref=ai show HEAD
```

### Test: amend preserves attribution

1. Commit with AI attribution
2. Amend:
```bash
git commit --amend --no-edit
```
3. Verify note was rewritten for the new SHA:
```bash
git notes --ref=ai show HEAD
```

### Test: stash preserves attribution

1. Make AI-attributed changes (checkpoint them but don't commit)
2. Stash:
```bash
git stash
```
3. Pop:
```bash
git stash pop
```
4. Commit and verify attribution is intact

### Test: fetch notes from remote

```bash
git-ai fetch-notes origin
git notes --ref=ai list
```

## Troubleshooting

### No attribution appearing

1. Check daemon is running: `git-ai bg status`
2. Check trace2 config: `git config --global trace2.eventTarget`
3. Check hook is installed: `cat .git/hooks/post-commit`
4. Enable debug output: `GIT_AI_DEBUG=1 git commit -m "test"`

### Daemon not receiving events

1. Check socket exists: `ls ~/.git-ai/internal/daemon/trace2.sock`
2. Check trace2 is configured: `git config --global --get trace2.eventTarget`
3. Test trace2 manually: `GIT_TRACE2_EVENT=1 git status 2>&1 | head`

### Wrong attribution (AI showing as human or vice versa)

1. Check working log exists before commit: `ls .git/ai/working_logs/$(git rev-parse HEAD)/`
2. Verify checkpoint is firing: `GIT_AI_DEBUG=1 git-ai checkpoint mock_ai test.txt`
3. Inspect the raw note: `git notes --ref=ai show HEAD`

### Post-commit hook not firing

1. Verify hook exists and is executable: `ls -la .git/hooks/post-commit`
2. Run manually: `.git/hooks/post-commit`
3. Check if another hook manager (husky, lefthook) is overriding it
