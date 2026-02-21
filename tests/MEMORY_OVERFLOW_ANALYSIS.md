# Memory Overflow Analysis - Issue #344

## Summary

Users report git-ai consuming 47-60GB+ RAM during git operations after long sessions with multiple AI agents/swarms. This analysis identifies 6 root causes and provides a replication test suite.

## Identified Culprits

### 1. Repeated full checkpoint re-reads during a single operation

**Severity: Critical**

A single `checkpoint::run()` call triggers `read_all_checkpoints()` at least **4 times**:

| Call Site | File | Line | Purpose |
|-----------|------|------|---------|
| `checkpoint::run()` | `checkpoint.rs` | 286 | Read existing checkpoints |
| `get_all_tracked_files()` | `checkpoint.rs` | 663 | Find files from checkpoint entries |
| `get_all_tracked_files()` | `checkpoint.rs` | 693 | Check if any AI checkpoints exist |
| `append_checkpoint()` | `repo_storage.rs` | 325 | Re-read all before appending one |

Each call:
1. Reads the entire JSONL file into a `String` (file size allocation)
2. Deserializes every line into `Checkpoint` structs (2-5x file size in memory)
3. Runs hash migration on all checkpoints

**Impact**: With a 1GB JSONL file, each read allocates ~2-3GB. Four reads = **8-12 GB minimum** per checkpoint operation.

### 2. `append_checkpoint` rewrites ALL checkpoints every time

**Severity: Critical**

```
repo_storage.rs:323 - append_checkpoint():
  1. Reads ALL existing checkpoints (line 325)
  2. Clones the new checkpoint (line 335)
  3. Pushes to vector (line 370)
  4. Prunes attributions across ALL (line 374)
  5. Writes ALL back to disk (line 377)
```

After K checkpoint appends, total I/O = K*(K+1)/2 = **O(K^2)**.

With 100 checkpoints of ~1MB each: total bytes written = ~5 GB cumulative.

### 3. Transcript data accumulation without bounds

**Severity: High**

Tools that "cannot refetch" transcripts keep them inline in checkpoints.jsonl:
- `mock_ai`, `opencode`, and any unknown/custom tools (line 344-363 of repo_storage.rs)

A single long agent conversation can produce:
- 200+ messages with code snippets = ~400KB per checkpoint
- `ToolUse` messages with full file contents in `serde_json::Value` = unbounded

With 50 checkpoints * 400KB transcripts = **20MB** JSONL file just from transcripts.
At scale: 200+ checkpoints from swarms = **500MB - 5GB** of transcript data alone.

### 4. Clone-heavy patterns in concurrent checkpoint processing

**Severity: Medium**

`get_checkpoint_entries()` (checkpoint.rs:1105) spawns concurrent tasks per file:
- `repo.clone()` (line 1183) - full Repository struct
- `working_log.clone()` (line 1184) - includes dirty_files HashMap
- `entries.clone()` (line 397) - duplicates all WorkingLogEntry data
- `checkpoint.clone()` (line 446) - duplicates entire Checkpoint including transcript

With 30 concurrent file tasks and large checkpoints, this creates 30x copies of significant data structures.

### 5. VirtualAttributions loads everything into memory simultaneously

**Severity: Medium**

`from_just_working_log()` (virtual_attribution.rs:295):
- Reads ALL checkpoints (line 302) - yet another full deserialization
- Reads ALL initial attributions
- Loads ALL file contents into `HashMap<String, String>` (line 221)
- Builds character and line attribution maps for every tracked file

For a repo with 100 tracked files averaging 10KB each = 1MB of file content + attribution overhead.

### 6. Post-commit re-reads everything after pre-commit already did

**Severity: High**

The commit flow:
1. **Pre-commit hook** -> `checkpoint::run()` -> 4x `read_all_checkpoints()`
2. Git commit executes
3. **Post-commit hook** -> `post_commit()` (line 69) -> `read_all_checkpoints()`
4. `update_prompts_to_latest()` (line 78) -> processes all checkpoints
5. `write_all_checkpoints()` (line 80) -> serializes all
6. `VirtualAttributions::from_just_working_log()` (line 302) -> reads ALL again

Total: **6+ full deserializations** of the entire checkpoint file per commit.

## Scaling Analysis

| JSONL Size | Single Read Memory | Reads Per Commit | Peak Memory |
|-----------|-------------------|------------------|-------------|
| 10 MB | ~20-30 MB | 6 | ~120-180 MB |
| 100 MB | ~200-300 MB | 6 | ~1.2-1.8 GB |
| 500 MB | ~1-1.5 GB | 6 | ~6-9 GB |
| 1 GB | ~2-3 GB | 6 | ~12-18 GB |
| 5 GB | ~10-15 GB | 6 | ~60-90 GB |

The 60-90 GB projection for 5 GB JSONL files matches the user reports of 47-60 GB.

## Replication

The test suite in `memory_overflow_replication.rs` contains 6 tests:

1. **test_memory_overflow_append_checkpoint_quadratic_growth** - Shows O(N^2) growth from repeated appends
2. **test_memory_overflow_large_transcripts_accumulation** - Shows impact of large transcripts
3. **test_memory_overflow_multiplied_checkpoint_reads** - Demonstrates 4+ reads per operation
4. **test_memory_overflow_realistic_multi_agent_session** - End-to-end multi-agent simulation
5. **test_memory_overflow_scaling_projection** - Projects memory at various JSONL sizes
6. **test_memory_overflow_append_rewrite_all_pattern** - Measures write amplification

Run: `cargo test --test memory_overflow_replication -- --nocapture`

## Proposed Fix Plan

### Phase 1: Quick wins (biggest impact)

1. **Cache checkpoint reads within a single operation**
   - Add a checkpoint cache to `PersistedWorkingLog` (e.g., `RefCell<Option<Vec<Checkpoint>>>`)
   - Invalidate on write. Eliminates 3 of 4 redundant reads per `checkpoint::run()`
   - Estimated impact: **4x reduction** in peak memory during checkpoint operations

2. **Append-only checkpoint writes**
   - Change `append_checkpoint()` to actually append a single JSONL line to the file
   - Instead of: read all -> add one -> write all
   - Do: open file in append mode -> write one line
   - Estimated impact: **O(N) instead of O(N^2)** I/O

### Phase 2: Memory efficiency

3. **Streaming checkpoint reads**
   - Use `BufReader` to read line-by-line instead of `fs::read_to_string`
   - Parse each line individually without holding the entire file string
   - Estimated impact: **~50% reduction** in per-read memory (eliminate string copy)

4. **Strip transcripts from in-memory checkpoints when not needed**
   - Most read paths only need entries + metadata, not transcripts
   - Add a `read_all_checkpoints_without_transcripts()` variant
   - Only load transcripts in `post_commit` where `update_prompts_to_latest()` needs them
   - Estimated impact: **80-95% reduction** for transcript-heavy workloads

### Phase 3: Structural improvements

5. **Shared references instead of clones in concurrent processing**
   - Use `Arc<Vec<Checkpoint>>` instead of re-reading for each concurrent task
   - Pass `Arc<Checkpoint>` instead of cloning checkpoints

6. **Checkpoint file size limits with rotation**
   - Set a max checkpoint file size (e.g., 100MB)
   - When exceeded, archive older checkpoints or discard non-essential data
   - Add warnings when approaching limits

7. **Lazy-load VirtualAttributions**
   - Only load file contents when actually needed for a specific file
   - Use memory-mapped files or streaming for large file sets
