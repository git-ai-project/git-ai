# Performance Work Status

## What We've Built

### 1. ✅ Subprocess Instrumentation Framework
**Location:** `src/perf/subprocess_instrumentation.rs`

**What it does:**
- Auto-instruments ALL `exec_git()` calls when `GIT_AI_INSTRUMENT_SUBPROCESSES=1`
- Tracks: count, duration, command type, critical path status
- Outputs: Human-readable summary or JSON for automation

**Real data from `git-ai status`:**
```
15 subprocess calls
13.7ms in subprocesses (91% of 14.9ms total)
9x rev-parse (60%)
2x symbolic-ref (13%)
```

**Files:**
- `src/perf/subprocess_instrumentation.rs` - Framework
- `src/git/repository.rs` - Auto-instrumentation in `exec_git*` functions
- `src/main.rs` - Environment variable handling, report printing
- `SUBPROCESS_INSTRUMENTATION.md` - User docs
- `scripts/perf-baseline.sh` - Collection script

### 2. ✅ Fast Git Implementation (Direct .git Parsing)
**Location:** `src/git/impl/`

**What it does:**
- Parses `.git/HEAD`, `.git/refs/`, `.git/packed-refs` directly
- Reads loose objects with zlib decompression
- Avoids subprocess for ~80% of ref lookups

**Modules:**
- `refs.rs` - HEAD, loose refs, packed-refs (17 tests, all pass)
- `objects.rs` - Loose blobs/commits (8 tests, all pass)
- `config.rs` - .git/config parsing (7 tests, all pass)

**Not implemented (fallback to git CLI):**
- Packfiles (too complex)
- Worktrees (different structure)
- Complex rev-parse syntax

**Expected Windows savings:**
- ref resolution: 50ms → 0.3ms (**166x faster**)
- Per `git-ai status`: Save ~11 of 15 subprocess calls

### 3. 📝 Documentation
- `PERF_ANALYSIS_PLAN.md` - Overall strategy
- `INSTRUMENTATION_FINDINGS.md` - Test results
- `SUBPROCESS_INSTRUMENTATION.md` - How to use instrumentation
- `docs/perf/windows-baseline-plan.md` - Baseline collection plan
- `src/git/impl/README.md` - Fast impl usage

## What's NOT Done (Next Steps)

### Phase 1: Collect Windows Baseline ⏳
**Need:** Run `scripts/perf-baseline.sh` on Windows CI

**Decision point:** Only proceed if subprocess overhead > 50ms on Windows

### Phase 2: Integrate Fast Implementation ⏳
**If Windows baseline shows problem:**
1. Add feature flag `fast-git-impl` (default on Windows)
2. Update `Repository::head()` to try fast path first:
   ```rust
   pub fn head(&self) -> Result<Reference> {
       #[cfg(feature = "fast-git-impl")]
       if let Some(refname) = FastGitReader::new(&self.git_dir).try_read_head_symbolic()? {
           return Ok(Reference { repo: self, ref_name: refname });
       }
       
       // Fallback to git CLI
       exec_git(&["symbolic-ref", "HEAD"])
   }
   ```
3. Same for `resolve_ref()` (9 calls in status!)
4. Benchmark before/after on Windows

### Phase 3: Batching (Alternative/Additional Optimization) ⏳
Instrumentation shows "sequential 8 calls" to rev-parse.

**Opportunity:**
```rust
// Before: 8 subprocess calls
for sha in shas {
    exec_git(&["rev-parse", sha])?;
}

// After: 1 subprocess call
let input = shas.join("\n");
exec_git_stdin(&["cat-file", "--batch-check"], input)?;
```

### Phase 4: Caching ⏳
Cache results of read-only queries:
```rust
struct Repository {
    head_cache: OnceCell<String>,
}
```

## Current Status

✅ **Framework complete and tested**
- Instrumentation works
- Fast impl works
- All tests pass

⏸️ **Waiting on data**
- Need Windows baseline numbers
- Then make go/no-go decision on integration

## Files Modified/Created

### New Files
- `src/perf/subprocess_instrumentation.rs`
- `src/perf/mod.rs`
- `src/git/impl/mod.rs`
- `src/git/impl/refs.rs`
- `src/git/impl/objects.rs`
- `src/git/impl/config.rs`
- `src/git/impl/README.md`
- `scripts/perf-baseline.sh`
- `tests/integration/subprocess_instrumentation.rs`
- 5 documentation files

### Modified Files
- `src/lib.rs` - Added `perf` module
- `src/git/mod.rs` - Added `impl` module
- `src/git/repository.rs` - Auto-instrumentation in `exec_git*`
- `src/main.rs` - Environment variable handling
- `tests/integration/main.rs` - Added test module
- `Cargo.toml` - Added `flate2` dependency (already in tree via `zip`)
- Various clippy auto-fixes

## Questions to Answer

1. **What's the actual Windows subprocess cost?**
   - Run baseline script
   - Compare to Linux

2. **Does fast impl actually help?**
   - Benchmark integrated version on Windows
   - Measure before/after

3. **Which optimization has best ROI?**
   - Fast impl? (big change, big potential)
   - Batching? (medium change, medium gain)
   - Caching? (small change, small gain)

## Recommendation

**Don't commit yet.** We have:
1. ✅ Working instrumentation (good to commit)
2. ✅ Working fast impl (good to commit)
3. ❌ No Windows data (need before committing)
4. ❌ Not integrated (need data before integrating)

**Next action:** Run baseline on Windows, analyze results, then decide strategy.
