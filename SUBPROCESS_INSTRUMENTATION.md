# Subprocess Instrumentation

This document describes the subprocess instrumentation system for analyzing git subprocess spawn patterns and identifying performance bottlenecks.

## Overview

Git-ai launches git subprocesses for all operations. On Windows especially, process creation overhead is significant. The instrumentation system helps us:

1. **Count** subprocess calls by category and command
2. **Measure** time spent in each subprocess
3. **Identify** batching opportunities
4. **Track** critical path operations
5. **Analyze** call patterns and velocity

## Usage

### Enable Instrumentation

Set the environment variable before running git-ai:

```bash
export GIT_AI_INSTRUMENT_SUBPROCESSES=1
```

### Run Operations

Execute any git-ai or git operations as usual:

```bash
# Test checkpoint operations
git-ai checkpoint human

# Test commit operations  
git commit -m "test"

# Test status operations
git-ai status

# Test blame operations
git-ai blame README.md
```

### View Results

By default, a human-readable summary is printed to stderr at the end of execution:

```
=== Subprocess Instrumentation Report ===
Total time elapsed: 2.5s
Total subprocess calls: 47
Total time in subprocesses: 1.8s
Average subprocess duration: 38ms
Time spent in subprocesses: 72%

--- Calls by Category ---
  repository_query     18 (38%)
  authorship_notes     12 (26%)
  diff_operation        8 (17%)
  commit_hook           5 (11%)
  checkpoint            4 (8%)

--- Calls by Git Command ---
  rev-parse            12 (26%)
  show-ref              6 (13%)
  notes                 8 (17%)
  diff                  7 (15%)
  ...

--- Critical Path Analysis ---
Critical path calls: 15 (32%)
Critical path time: 850ms (47%)

--- Potential Batching Opportunities ---
  repository_query     rev-parse (sequential 8 calls)
  authorship_notes     notes (sequential 6 calls)
```

### JSON Output

For automated analysis or external tools, use JSON format:

```bash
export GIT_AI_INSTRUMENT_JSON=1
```

This outputs structured JSON with all invocations and timing data.

## Adding Instrumentation to Code

### Basic Usage

For any git operation, use `exec_git_with_context` instead of `exec_git`:

```rust
use crate::git::repository::exec_git_with_context;
use crate::perf::subprocess_instrumentation::{SubprocessContext, SubprocessCategory};

// Old way (no instrumentation)
let output = exec_git(&["rev-parse".to_string(), "HEAD".to_string()])?;

// New way (with instrumentation)
let ctx = SubprocessContext::new(SubprocessCategory::RepositoryQuery, "rev-parse")
    .critical_path(true)  // This is user-blocking
    .label("get-head-commit");

let output = exec_git_with_context(
    &["rev-parse".to_string(), "HEAD".to_string()],
    ctx
)?;
```

### Context Categories

Choose the appropriate category for your operation:

- **Checkpoint** - Operations during `git-ai checkpoint`
- **CommitHook** - Operations in pre/post-commit hooks
- **RebaseHook** - Operations in rebase/cherry-pick hooks
- **StatusDisplay** - Operations for `git-ai status`
- **BlameDisplay** - Operations for `git-ai blame`
- **LogDisplay** - Operations for `git-ai log`
- **AuthorshipNotes** - Note read/write operations
- **RepositoryQuery** - rev-parse, show-ref, etc.
- **ObjectRead** - cat-file, show, etc.
- **DiffOperation** - diff, diff-tree operations
- **RefUpdate** - update-ref, branch operations
- **DaemonBackground** - Background daemon operations
- **Other** - Uncategorized

### Context Attributes

- **critical_path(bool)** - Is this blocking user interaction?
- **read_only(bool)** - Is this a read-only operation (default: true)?
- **label(String)** - Optional grouping label for related calls
- **stack_depth(usize)** - Nesting level (for detecting recursive patterns)

### Example: Instrumenting a Module

```rust
// In src/commands/status.rs

use crate::perf::subprocess_instrumentation::{SubprocessContext, SubprocessCategory};
use crate::git::repository::exec_git_with_context;

pub fn get_status() -> Result<Status, GitAiError> {
    // This is user-blocking, so mark as critical path
    let ctx = SubprocessContext::new(SubprocessCategory::StatusDisplay, "status")
        .critical_path(true)
        .label("main-status");
    
    let output = exec_git_with_context(&["status".to_string(), "--porcelain".to_string()], ctx)?;
    
    // ... parse output
}
```

## Analyzing Results

### High Call Counts

If a category shows high call counts, investigate:
- Can we batch multiple operations into one subprocess?
- Can we cache results?
- Are we calling redundantly?

### Sequential Patterns

The "Batching Opportunities" section shows commands called sequentially (within 10ms). These are prime candidates for:
- `git rev-list --stdin` batching
- `git cat-file --batch` operations
- Cached lookups

### Critical Path Time

Operations marked `critical_path(true)` directly impact user-perceived latency. Focus optimization on these first.

### Platform-Specific Analysis

On Windows, compare subprocess counts and timings vs. Linux/macOS:

```bash
# Linux
GIT_AI_INSTRUMENT_SUBPROCESSES=1 ./run_benchmark.sh > linux_results.json

# Windows  
$env:GIT_AI_INSTRUMENT_SUBPROCESSES=1
.\run_benchmark.ps1 > windows_results.json
```

## Migration Strategy

1. **Phase 1: Instrument hot paths** (checkpoint, commit hooks, status)
2. **Phase 2: Collect baseline metrics** (run full test suite with instrumentation)
3. **Phase 3: Identify quick wins** (sequential calls, redundant queries)
4. **Phase 4: Implement batching** (use git batch modes)
5. **Phase 5: Add caching** (for read-only, frequently-called operations)

## Next Steps

Based on instrumentation data, we can:

1. **Batch ref lookups** - Use `git for-each-ref` instead of multiple `show-ref`
2. **Cache rev-parse results** - Many operations query the same commits repeatedly
3. **Batch note operations** - Use `git fast-import` for bulk note writes
4. **Lazy initialization** - Defer non-critical operations
5. **Parallel execution** - Run independent queries concurrently on Windows

## Testing

Run the test suite with instrumentation enabled:

```bash
GIT_AI_INSTRUMENT_SUBPROCESSES=1 task test
```

This will show subprocess patterns during tests, helping identify optimization opportunities in test scenarios that mirror real usage.
