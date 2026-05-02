# Git-AI Performance Analysis & Optimization Plan

## Problem Statement

Git-AI launches git subprocesses liberally, which has critical performance impact on Windows where process creation is ~10-100x slower than Unix systems. We need to:

1. Identify all subprocess spawn points
2. Measure spawn frequency, timing, and patterns
3. Reduce, batch, or cache subprocess calls where possible
4. Establish ongoing performance monitoring

## Phase 1: Instrumentation (✅ COMPLETED)

### What We Built

1. **Instrumentation Framework** (`src/perf/subprocess_instrumentation.rs`)
   - Tracks subprocess calls with context
   - Categories: Checkpoint, CommitHook, RebaseHook, Status, Blame, Log, AuthorshipNotes, RepositoryQuery, ObjectRead, DiffOperation, RefUpdate, DaemonBackground, Other
   - Metrics: call count, duration, critical path status, read-only flag
   - Reports: human-readable summary and JSON format

2. **Integration Points** (`src/git/repository.rs`)
   - Added `*_with_context` variants to all `exec_git*` functions:
     - `exec_git_with_context()`
     - `exec_git_with_profile_and_context()`
     - `exec_git_allow_nonzero_with_profile_and_context()`
     - `exec_git_stdin_with_profile_and_context()`
   - Existing code continues to work unchanged
   - New code can opt-in to instrumentation by using `_with_context` variants

3. **Activation** (`src/main.rs`)
   - Enable via environment variable: `GIT_AI_INSTRUMENT_SUBPROCESSES=1`
   - Output format control: `GIT_AI_INSTRUMENT_JSON=1` for JSON
   - Prints report to stderr at program exit

### Usage

```bash
# Enable instrumentation
export GIT_AI_INSTRUMENT_SUBPROCESSES=1

# Run operations
git commit -m "test"
git-ai status
git-ai checkpoint human

# See report at end of execution
```

## Phase 2: Data Collection (NEXT)

### Subprocess Inventory

Based on code analysis, we have **161 `exec_git` calls** across **20 source files**:

**High-frequency areas:**
- `src/git/repository.rs` - 90+ calls (core git operations)
- `src/authorship/rebase_authorship.rs` - Heavy note rewriting
- `src/git/refs.rs` - Note operations
- `src/commands/blame.rs` - Blame display with batched operations
- `src/git/status.rs` - Status parsing
- `src/git/diff_tree_to_tree.rs` - Diff operations

### Measurement Campaign

Run instrumentation across common workflows:

1. **Checkpoint workflow**
   ```bash
   # Create test repo
   # Edit file
   git-ai checkpoint human /path/to/file
   # Edit file again  
   git-ai checkpoint mock_ai /path/to/file
   git add .
   git commit -m "test"
   ```

2. **Status workflow**
   ```bash
   git-ai status
   ```

3. **Blame workflow**
   ```bash
   git-ai blame src/main.rs
   ```

4. **Rebase workflow** (expensive on Windows)
   ```bash
   git rebase -i HEAD~10
   ```

5. **Large commit workflow**
   ```bash
   # Commit with many files
   # Monitor authorship note generation
   ```

### Metrics to Collect

- Total subprocess count per workflow
- Time distribution (what % of time is subprocess overhead?)
- Sequential patterns (multiple calls in quick succession)
- Redundant calls (same operation multiple times)
- Critical path calls (user-blocking)

## Phase 3: Quick Wins (TODO)

### Identify Optimization Opportunities

Based on instrumentation data, look for:

1. **Sequential same-command calls**
   - Multiple `rev-parse` → single `rev-parse` with multiple args
   - Multiple `show-ref` → single `for-each-ref`
   - Multiple `cat-file` → `cat-file --batch`

2. **Redundant queries**
   - HEAD commit queried multiple times in same operation
   - Same ref looked up repeatedly
   - Repeated note reads for same commit

3. **Cacheable operations**
   - Read-only repository queries (workdir, bare check, config)
   - Commit metadata (parents, summary, author)
   - Ref resolution (HEAD, branch names)

4. **Batching candidates**
   - Note writes (use `git fast-import` for bulk operations)
   - Object reads (use `cat-file --batch`)
   - Diff operations (use `diff-tree` with multiple commits)

### Implementation Strategies

**Strategy 1: Batch Mode Operations**
```rust
// Before: N subprocesses
for oid in oids {
    let output = exec_git(&["cat-file", "-p", oid])?;
}

// After: 1 subprocess with batch mode
let batch_input = oids.join("\n");
let output = exec_git_stdin(&["cat-file", "--batch"], batch_input.as_bytes())?;
```

**Strategy 2: Caching Layer**
```rust
// Repository-level cache for expensive queries
pub struct Repository {
    // ... existing fields
    head_cache: OnceCell<String>,
    workdir_cache: OnceCell<PathBuf>,
}

impl Repository {
    pub fn head_commit(&self) -> Result<String, GitAiError> {
        self.head_cache.get_or_try_init(|| {
            // exec_git call here
        }).cloned()
    }
}
```

**Strategy 3: Lazy Initialization**
```rust
// Don't query unless actually needed
pub struct Commit {
    oid: String,
    summary: OnceCell<String>,  // Loaded on first access
}
```

**Strategy 4: Parallel Execution**
```rust
// Independent queries can run concurrently
let (result_a, result_b) = rayon::join(
    || exec_git(&["rev-parse", "HEAD"]),
    || exec_git(&["show-ref", "--heads"]),
);
```

## Phase 4: Benchmarking (TODO)

### Baseline Metrics

Before optimization, establish baselines:

1. **Windows baseline** (critical)
   - Cold start: git commit time
   - Warm: subsequent commits
   - Large repo (1000+ files)
   - Small repo (10 files)

2. **Linux/macOS baseline** (comparison)
   - Same metrics as Windows
   - Compare subprocess overhead impact

### Optimization Targets

- **Target 1:** Reduce subprocess count by 50% for commit workflow
- **Target 2:** Reduce commit hook time by 30% on Windows
- **Target 3:** Batch sequential note operations (10+ calls → 1-2 calls)
- **Target 4:** Cache repository queries (eliminate redundant rev-parse)

### Benchmark Suite

Create `tests/integration/perf_benchmark.rs`:
```rust
#[test]
fn benchmark_commit_workflow_windows() {
    let repo = TestRepo::new();
    // Set up files
    
    let start = Instant::now();
    
    // Pre-checkpoint
    repo.git_ai(&["checkpoint", "human", "file.txt"]).unwrap();
    
    // Post-checkpoint  
    repo.git_ai(&["checkpoint", "mock_ai", "file.txt"]).unwrap();
    
    // Commit
    repo.stage_all_and_commit("Test").unwrap();
    
    let duration = start.elapsed();
    
    // Assert subprocess count
    assert!(subprocess_count < 50, "Too many subprocesses");
    
    // Assert timing (on Windows)
    #[cfg(windows)]
    assert!(duration < Duration::from_secs(2), "Commit took too long");
}
```

## Phase 5: Monitoring (TODO)

### Ongoing Performance Tracking

1. **CI Integration**
   - Run instrumented benchmarks on Windows CI
   - Track metrics over time
   - Alert on regressions

2. **Performance Budget**
   - Subprocess count limits per operation
   - Duration limits for critical paths
   - CI fails if budget exceeded

3. **Documentation**
   - Document subprocess call costs
   - Guidelines for new code (when to batch, cache, etc.)
   - Performance best practices

## Implementation Checklist

- [x] Create instrumentation framework
- [x] Integrate into exec_git functions
- [x] Add environment variable activation
- [x] Document usage
- [ ] Instrument 5-10 hot paths with context
- [ ] Run measurement campaign (Windows & Linux)
- [ ] Analyze results, identify top 10 optimizations
- [ ] Implement batching for note operations
- [ ] Add caching layer for repository queries
- [ ] Create benchmark suite
- [ ] Set up CI performance tracking
- [ ] Document performance guidelines

## Files Modified

- `src/perf/subprocess_instrumentation.rs` (new)
- `src/perf/mod.rs` (new)
- `src/lib.rs` (added perf module)
- `src/git/repository.rs` (added *_with_context functions, instrumentation)
- `src/main.rs` (environment variable handling, report printing)
- `SUBPROCESS_INSTRUMENTATION.md` (documentation)
- `PERF_ANALYSIS_PLAN.md` (this file)

## Next Steps

1. **Add instrumentation to hot paths** - Start with checkpoint, commit hooks, status
2. **Collect baseline data** - Run instrumented workflows on Windows and Linux
3. **Analyze patterns** - Use JSON output to identify batching opportunities
4. **Prioritize optimizations** - Focus on Windows critical paths first
5. **Implement & measure** - Track improvements with before/after metrics
