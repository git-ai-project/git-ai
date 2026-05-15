# MDM (Managed Device Management)

Zero-config hook installation for AI coding agents.

## What it does

Detects which AI coding tools are installed on the system and installs checkpoint hooks into their configuration files so that git-ai receives notifications when AI edits files.

## Supported agents

| Agent | Detect dir | Config file | Hook format |
|-------|-----------|-------------|-------------|
| Cursor | `.cursor` | `.cursor/hooks/hooks.json` | CursorStyle |
| Claude Code | `.claude` | `.claude/settings.json` | ClaudeStyle |
| Windsurf | `.windsurf` | `.windsurf/hooks/hooks.json` | CursorStyle |
| Amp | `.amp` | `.amp/hooks/hooks.json` | PascalCursorStyle |
| Codex | `.codex` | `.codex/hooks/hooks.json` | PascalCursorStyle |
| Gemini | `.gemini` | — | Not yet supported |
| Pi | `.pi` | — | Not yet supported |
| OpenCode | `.opencode` | — | Not yet supported |
| Droid | `.droid` | — | Not yet supported |
| GitHub Copilot | `.github-copilot` | — | Not yet supported |
| Firebender | `.firebender` | — | Not yet supported |
| Continue | `.continue` | — | Not yet supported |

## Design

A single static config table (`AGENT_INSTALL_CONFIGS`) drives all behavior — detection, installation, status reporting. Adding a new agent is one table entry.

### Hook formats

- **CursorStyle**: `{ "hooks": { "preToolUse": [...], "postToolUse": [...] } }`
- **PascalCursorStyle**: Same structure but `PreToolUse`/`PostToolUse` (pascal case keys)
- **ClaudeStyle**: `{ "hooks": { "PreToolUse": [{ "type": "command", ... }] } }` with `matcher` and `timeout` fields

### Config merging

Installation is non-destructive: existing hooks, settings, and third-party integrations are preserved. The merge logic reads existing JSON, appends the git-ai hook entry if not already present, and writes back pretty-printed.

## Public API

- `detect_installed()` — which agents are present on this machine
- `install_hooks(tool)` — install hooks for a specific agent
- `install_all()` — install hooks for all detected agents
- `status()` — report detection + installation state for all known agents
