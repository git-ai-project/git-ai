# Transcripts

Incremental transcript reader for AI agent session files.

## What it does

Reads agent conversation/session files to extract prompt metadata, model info, and conversation history. This powers richer attribution — knowing which model and which prompt generated which code.

## Architecture

Two generic reading strategies cover all 11 supported agents:

| Strategy | Function | Used by |
|----------|----------|---------|
| Byte-offset JSONL | `read_jsonl_incremental()` | Cursor, Claude, Codex, Gemini, Windsurf, Droid, Pi, GitHub Copilot (9 agents) |
| Record-index JSON array | `read_json_array_incremental()` | Amp, Continue CLI (2 agents) |

## Key types

- `TranscriptBatch` — a batch of parsed events plus the resume position
- `TranscriptError` — not-found, I/O, or parse errors
- `AgentTranscriptConfig` — per-agent discovery and format configuration
- `DiscoveryStrategy` — how to find transcript files (scan known dirs vs. hook payload)

## Incremental reading

Both readers support incremental consumption:
- JSONL: seeks to a byte offset, reads N lines, returns new offset
- JSON array: skips N records, reads next batch, returns new index

This allows the daemon to poll for new transcript data without re-reading entire files.

## Discovery

`discover_sessions(tool)` scans known directories (relative to `$HOME`) for transcript files matching the agent's expected location and extension. Agents that provide transcript paths via hook payloads use `FromHookPayload` strategy (no scanning needed).
