# Windows Performance Baseline Collection Plan

## Goal
Establish concrete performance baselines for common git-ai operations on Windows vs Linux/macOS to quantify the subprocess overhead problem.

## Current Data (Linux)

From instrumentation of `git-ai status`:
- **15 subprocess calls**
- **13.7ms in subprocesses** (91% of total 14.9ms)
- **9x rev-parse** (60% of calls)
- **2x symbolic-ref** (13%)
- Average subprocess: **910µs**

## Hypothesis

Windows process creation is **10-100x slower** than Linux:
- Linux subprocess: ~1ms
- Windows subprocess: **10-100ms**

If true, Windows `git-ai status` would take:
- 15 calls × 10ms = **150ms** (best case)
- 15 calls × 100ms = **1.5 seconds** (worst case)

vs Linux: **15ms**

## Benchmarks to Run

### Test Scenarios

1. **git-ai status** (no checkpoints)
   - Baseline operation
   - Lots of rev-parse calls

2. **git-ai checkpoint human file.txt**
   - Pre-edit checkpoint
   - Diff operations

3. **git commit** (with git-ai hooks)
   - Post-commit hook
   - Authorship note generation

4. **git-ai blame file.txt**
   - Heavy git operations
   - Note reading

5. **Large commit** (100 files)
   - Authorship processing
   - Note writing bottleneck

### Metrics to Collect

For each scenario, collect:
```json
{
  "platform": "windows|linux|macos",
  "operation": "status|checkpoint|commit|blame",
  "total_duration_ms": 0,
  "subprocess_count": 0,
  "subprocess_duration_ms": 0,
  "subprocess_percentage": 0,
  "by_command": {
    "rev-parse": {"count": 0, "total_ms": 0, "avg_ms": 0},
    "symbolic-ref": {"count": 0, "total_ms": 0, "avg_ms": 0}
  }
}
```

## Test Script

```bash
#!/bin/bash
# Run on Windows, Linux, macOS

export GIT_AI_INSTRUMENT_SUBPROCESSES=1
export GIT_AI_INSTRUMENT_JSON=1

# Setup test repo
cd /tmp
rm -rf perf-test
mkdir perf-test
cd perf-test
git init
git config user.email "test@test.com"
git config user.name "Test"

# Test 1: Empty status
echo "line 1" > test.txt
git add test.txt
git commit -m "initial"
git-ai status 2>&1 | grep "^{" > baseline-status.json

# Test 2: Checkpoint
echo "line 2" >> test.txt
git-ai checkpoint human test.txt 2>&1 | grep "^{" > baseline-checkpoint.json

# Test 3: Commit
git add test.txt
git commit -m "test" 2>&1 | grep "^{" > baseline-commit.json

# Test 4: Blame
git-ai blame test.txt 2>&1 | grep "^{" > baseline-blame.json

# Test 5: Large commit
for i in {1..100}; do
  echo "file $i" > "file$i.txt"
done
git add .
git commit -m "large commit" 2>&1 | grep "^{" > baseline-large-commit.json
```

## Decision Matrix

After collecting data:

### If subprocess overhead < 50ms on Windows
→ **Optimization not worth it yet**
→ Focus on other bottlenecks

### If subprocess overhead 50-200ms on Windows
→ **Consider gix for Tier 1 operations** (rev-parse, symbolic-ref)
→ ROI: Medium effort, medium gain

### If subprocess overhead > 200ms on Windows
→ **Gix replacement is critical**
→ ROI: High effort, high gain
→ Could be 10x speedup on Windows

## Gix Integration Plan (If Needed)

### Phase 1: Proof of Concept
- Add gix dependency
- Replace just `rev-parse` in one location
- Benchmark: subprocess vs gix
- Measure actual speedup on Windows

### Phase 2: Tier 1 Replacements
```rust
// High-frequency, zero-risk operations
- rev-parse HEAD/refs       → gix: repo.rev_parse()
- symbolic-ref HEAD         → gix: repo.head_name()
- cat-file blob             → gix: repo.find_object()
```

### Phase 3: Tier 2 if Needed
```rust
- status --porcelain        → gix: repo.status()
- diff (simple)             → gix: repo.diff()
```

### Keep CLI For
- All write operations (hooks must fire)
- Complex authorship operations
- Operations where git behavior varies by version

## Action Items

- [ ] Run baseline script on Windows CI
- [ ] Run baseline script on Linux CI
- [ ] Run baseline script on macOS CI (if available)
- [ ] Analyze JSON output, compare platforms
- [ ] Calculate subprocess overhead multiplier (Windows vs Linux)
- [ ] Make go/no-go decision on gix integration
- [ ] If yes: Create gix integration plan with target operations
