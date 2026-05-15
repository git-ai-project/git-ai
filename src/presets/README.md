# Presets

Unified agent preset system for parsing hook payloads from AI coding agents.

## What it does

Each AI coding agent (Cursor, Claude Code, Copilot, etc.) sends hook events in its own format when it edits files. Presets parse these diverse payloads into a standardized `ParsedHookEvent` that the checkpoint processor consumes.

## Architecture

Table-driven: a static config table maps agent names to their payload parsing logic. Adding support for a new agent means adding one entry with its field mappings.

## Key types

- `ParsedHookEvent` — normalized output: file paths, event type (pre/post edit), session ID, model info
- `AgentPreset` — parsing configuration for one agent

## Modules

| File | Purpose |
|------|---------|
| `mod.rs` | Preset table, parsing dispatch, `ParsedHookEvent` definition |
| `tool_classification.rs` | Classifies agent tool calls as file-editing or non-editing |

## Data flow

```
Agent hook JSON (stdin) → preset parser → ParsedHookEvent → checkpoint processor
```

## Tool classification

Not all agent tool calls edit files. `tool_classification.rs` determines whether a given tool invocation (e.g., "write_file", "run_terminal_command", "read_file") is a file-editing operation that should trigger attribution tracking. Non-editing tools are ignored to avoid false attributions.
