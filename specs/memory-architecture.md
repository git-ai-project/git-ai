# Memory & Streaming Architecture — git-ai Checkpoint/Transcript Subsystem

> Last updated: 2026-04-04
> Related spec: specs/runaway-memory-plan.md

## Problem Statement

git-ai's checkpoint and transcript parsing paths exhibit ~5-8x memory amplification.
A 187 MB transcript produces ~1.2 GB peak RSS; a 307 MB checkpoint file produces ~1.78 GB
peak RSS with ~33s wall-clock time. This causes OOM risk, latency spikes, and poor UX on
long AI coding sessions.

## Subsystem Map

Three files form the hot path:

```
                     ┌──────────────────────────────────────┐
                     │  src/commands/checkpoint.rs           │
                     │                                      │
                     │  get_all_tracked_files()              │
                     │    ├── read_all_checkpoints() ×2      │  ← double read
                     │    └── file iteration                 │
                     │                                      │
                     │  get_checkpoint_entries()             │
                     │    ├── build_previous_file_state_maps │
                     │    ├── spawn per-file async tasks     │
                     │    │   └── get_checkpoint_entry_for_  │
                     │    │       file() (clones working_log)│  ← per-file clone
                     │    └── collect results                │
                     └───────────┬──────────────────────────┘
                                 │ calls
                     ┌───────────▼──────────────────────────┐
                     │  src/git/repo_storage.rs              │
                     │                                      │
                     │  append_checkpoint()                  │
                     │    ├── read_all_checkpoints()         │  ← full file read
                     │    ├── push new checkpoint            │
                     │    ├── prune_old_char_attributions()  │
                     │    └── write_all_checkpoints()        │  ← full file rewrite
                     │                                      │
                     │  read_all_checkpoints()               │
                     │    ├── fs::read_to_string()           │  ← entire file in memory
                     │    ├── parse each JSONL line          │
                     │    ├── hash migration (creates 2nd    │
                     │    │   Vec of all checkpoints)        │  ← 2x Vec allocation
                     │    └── return Vec<Checkpoint>         │
                     │                                      │
                     │  write_all_checkpoints()              │
                     │    ├── serialize all to Vec<String>   │
                     │    └── join + fs::write               │
                     └──────────────────────────────────────┘

                     ┌──────────────────────────────────────┐
                     │  src/commands/checkpoint_agent/       │
                     │           agent_presets.rs            │
                     │                                      │
                     │  Claude:  transcript_and_model_from_  │
                     │           claude_code_jsonl()         │
                     │    └── read_to_string → parse lines   │  ← full file in memory
                     │                                      │
                     │  Codex:   transcript_and_model_from_  │
                     │           codex_rollout_jsonl()       │
                     │    ├── read_to_string                 │  ← full file in memory
                     │    ├── parse ALL lines → Vec<Value>   │  ← intermediate Vec
                     │    └── iterate Vec twice              │
                     │                                      │
                     │  Droid:   transcript_and_model_from_  │
                     │           droid_jsonl()               │
                     │    └── read_to_string → parse lines   │  ← full file in memory
                     │                                      │
                     │  Windsurf: same pattern               │
                     │  Gemini:   read_to_string → from_str  │
                     │  Continue: read_to_string → from_str  │
                     └──────────────────────────────────────┘
```

## Memory Amplification Sources

### 1. Transcript Parsing (agent_presets.rs)

| Parser | Pattern | Memory Impact |
|--------|---------|---------------|
| Claude JSONL | `read_to_string` whole file, then iterate `.lines()` | 2x file size (string + parsed Values) |
| Codex JSONL | `read_to_string` + collect ALL into `Vec<Value>` + iterate twice | 3x file size (string + Vec + re-iteration) |
| Droid JSONL | `read_to_string` whole file, then iterate `.lines()` | 2x file size |
| Windsurf JSONL | `read_to_string` whole file | 2x file size |
| Gemini JSON | `read_to_string` + `from_str` | 2x file size |
| Continue JSON | `read_to_string` + `from_str` | 2x file size |

### 2. Checkpoint Storage (repo_storage.rs)

- `read_all_checkpoints()`: reads entire `checkpoints.jsonl` via `fs::read_to_string` (line 395).
  After parsing, performs hash migration by building `old_to_new_hash` map, then creates a
  **second `Vec<Checkpoint>`** (`migrated_checkpoints`) with all entries cloned/moved.
- `append_checkpoint()`: reads ALL existing checkpoints, pushes one new one, then rewrites
  the entire file. For N checkpoints, this is O(N) read + O(N) write per append.
- `write_all_checkpoints()`: serializes all checkpoints into `Vec<String>`, joins them, writes.

### 3. Checkpoint Command (checkpoint.rs)

- `get_all_tracked_files()` calls `read_all_checkpoints()` **twice** (lines 667 and 697).
  The second call re-reads the file just to check if any AI checkpoints exist.
- `get_checkpoint_entries()` receives `&[Checkpoint]` — this is already better than cloning,
  but `working_log.clone()` is called once per file in the async task spawn loop (line 1189).
  `PersistedWorkingLog` is a large struct with filesystem paths and optional data.

## Data Flow: Checkpoint Command

```
checkpoint command
  │
  ├── read_all_checkpoints() ─── checkpoints.jsonl ──→ Vec<Checkpoint>
  │
  ├── get_all_tracked_files()
  │     ├── read_all_checkpoints() ──── 2nd read (line 667)
  │     └── read_all_checkpoints() ──── 3rd read (line 697)
  │
  ├── save_current_file_states()
  │
  ├── get_checkpoint_entries()
  │     ├── build_previous_file_state_maps(checkpoints)
  │     └── for each file:
  │           ├── clone working_log
  │           └── spawn async → get_checkpoint_entry_for_file()
  │
  └── append_checkpoint()
        ├── read_all_checkpoints() ──── 4th read!
        ├── push new checkpoint
        ├── prune_old_char_attributions()
        └── write_all_checkpoints() ──── full rewrite
```

Total: `read_all_checkpoints()` is called **up to 4 times** per checkpoint command.

## Key Data Structures

### Checkpoint (working_log.rs)
```rust
pub struct Checkpoint {
    pub api_version: u32,
    pub kind: CheckpointKind,           // Human | AiAgent | AiTab
    pub entries: Vec<WorkingLogEntry>,   // per-file attribution data
    pub agent_id: Option<AgentId>,
    pub agent_metadata: Option<HashMap<String, serde_json::Value>>,
    pub transcript: Option<AiTranscript>,
    pub ts: u128,
}
```

### WorkingLogEntry (working_log.rs)
```rust
pub struct WorkingLogEntry {
    pub file: String,
    pub blob_sha: String,
    pub attributions: Vec<CharAttribution>,      // char-level precision (large!)
    pub line_attributions: Vec<LineAttribution>,  // line-level summaries
}
```

### AiTranscript (transcript.rs)
```rust
pub struct AiTranscript {
    messages: Vec<Message>,
}
// Message variants: User, Assistant, ToolUse, ToolResult, System, Summary
```

## Remediation Architecture (3 phases)

### Phase 0: Safety Rails
- Add `max_checkpoint_jsonl_bytes` (64 MB) and `max_transcript_bytes` (32 MB) config keys
- Check file metadata size before parsing; skip with warning if exceeded
- Never hard-fail `git commit`

### Phase 1: Reduce Redundant Work
- Pass checkpoints into `get_all_tracked_files()` as parameter (eliminate 2 re-reads)
- `append_checkpoint()`: serialize new entry, append to file (no full read+rewrite)
- Migrate hash in-place instead of creating second Vec

### Phase 2: Streaming Parsers
- All JSONL parsers: `BufReader` + `.lines()` instead of `read_to_string`
- Codex: eliminate `Vec<Value>` intermediate buffer
- `read_all_checkpoints()`: `BufReader` + line-by-line parsing
- Gemini/Continue: `serde_json::from_reader` instead of `from_str`

### Phase 3: Storage Refactor (future)
- Append-only checkpoint log + periodic compaction
- Hot working set vs cold history separation
- Near-linear checkpoint runtime in changed files, not history size

## Existing Progress

Branch `feat/streaming-transcript-parsing` (commit e2b68527) has:
- Claude, Codex, Windsurf, Droid, Gemini, Continue parsers converted to streaming
- Codex: eliminated `Vec<Value>`, uses two-pass streaming with file re-open for fallback
- E2E test script (`scripts/e2e_streaming_transcript_tests.py`)

**Not yet done:**
- Phase 0 (safety rails) — no size caps implemented
- Phase 1 (checkpoint command optimization) — no changes to checkpoint.rs or repo_storage.rs
- Phase 2 checkpoint JSONL streaming — `read_all_checkpoints` still uses `read_to_string`
- No integration of streaming work into main branch yet

## Target Metrics

| Scenario | Current | Target |
|----------|---------|--------|
| Transcript 187 MB | 1.20 GB RSS | <=720 MB (40% reduction) |
| Checkpoint 307 MB | 1.78 GB RSS, 33s | <=1.25 GB RSS (30%), <=25s (25%) |
