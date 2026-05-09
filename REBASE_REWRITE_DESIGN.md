# Rebase Attribution Rewrite - Design Document

## Problem Statement

Current rebase attribution implementation (~5K LOC) has production failures:
- Reflog regression errors when daemon state becomes inconsistent
- Wrapper state timeouts causing missing attribution data
- Complex caching/reconstruction logic that's hard to debug

## Core Principle

**Track hunks through transformations, not commits.**

A rebase is a transformation: `[Commits] -> [Commits]` where each commit may undergo changes to:
- refs, parentage, authorship, message, content
- Many-to-one (squash), one-to-many (split), one-to-zero (delete)

## Attribution Rules

### 1. Hunk Identity
For any hunk `h` in the original commits:
- **Copy**: `h` appears unchanged → attribution unchanged
- **Split**: `h` splits into `h1, h2, ...` → attribution splits proportionally
- **Delete**: `h` removed → attribution removed

### 2. Conflict Resolution
New hunks introduced during conflict resolution:
- **AI-attributed**: If created by AI while repo in conflicted state (checkpoint exists)
- **Human-attributed**: If created by Known Human while repo in conflicted state (checkpoint exists)
- **Untracked**: Otherwise (no checkpoint during conflict resolution)

### 3. Commit Scoping
Attribution for commit `C` includes ONLY changes visible in `git show C`.
- No "phantom" attributions for unchanged lines
- Clean diff-based scoping

### 4. Performance Bound
Additional git-ai overhead must be ≤50% of native git time.
- If `git rebase` takes 100s, git-ai processing ≤50s

## Implementation Strategy

### Phase 1: Core Transform Engine
Build hunk-level transformation tracker:

```rust
struct HunkTransform {
    original_commit: String,
    original_file: String,
    original_lines: Range<usize>,
    
    new_commit: Option<String>,  // None if deleted
    new_file: String,
    new_lines: Range<usize>,
    
    transform_type: TransformType,  // Copy, Split, Delete
}

enum TransformType {
    Copy,           // Exact copy
    Split(Vec<Range<usize>>),  // Split into multiple ranges
    Delete,         // Removed entirely
}
```

### Phase 2: Diff-Based Mapping
Use git's native diff algorithm to map hunks:

```rust
fn map_commit_hunks(
    repo: &Repository,
    original_commit: &str,
    new_commit: &str,
) -> Result<Vec<HunkTransform>, GitAiError> {
    // 1. Get diff between original and new commit trees
    // 2. For each hunk in original commit, find corresponding hunk(s) in new
    // 3. Build HunkTransform records
}
```

### Phase 3: Attribution Application
Apply transformations to existing attribution notes:

```rust
fn apply_transforms(
    original_notes: &[AuthorshipLog],
    transforms: &[HunkTransform],
) -> Vec<AuthorshipLog> {
    // For each original attestation (file + line range):
    // - Find matching HunkTransform
    // - Apply Copy/Split/Delete operation
    // - Generate new attestation in new commit
}
```

### Phase 4: Conflict Resolution Handler
Detect and attribute conflict resolution hunks:

```rust
fn attribute_conflict_resolution(
    repo: &Repository,
    conflicted_files: &[String],
    resolution_commit: &str,
) -> Result<HashMap<String, Vec<LineAttribution>>, GitAiError> {
    // Check working logs for checkpoints during conflict state
    // Attribute new hunks based on checkpoint data
    // Default to untracked if no checkpoint
}
```

## Testing Strategy

### Existing Tests Must Pass
- `rebase_determinism` (8 tests)
- `rebase_notes_durability` (11 tests)
- `rebase_production_failures` (18 tests - some have timing issues)

### No Overfitting
- Don't special-case test scenarios
- Implementation must handle general case
- Tests validate correctness, not drive implementation

## Migration Path

1. **Create new module**: `src/authorship/rebase_v3.rs`
2. **Feature flag**: `feature_flags.rebase_v3 = false` (default off)
3. **Parallel run**: Test v3 alongside existing implementation
4. **Gradual rollout**: Enable for subset of users
5. **Deprecate old**: Remove `rebase_authorship.rs` after validation

## Success Metrics

- All existing tests pass
- Reflog regression errors eliminated
- No wrapper state dependencies
- Performance within 50% overhead
- Production attribution loss <1%

## Non-Goals (Out of Scope)

- Filter-branch support
- Fixup commit handling
- ReReRe integration

These can be added later if needed.

---

## Next Steps

1. Implement `HunkTransform` core data structure
2. Build diff-based hunk mapper
3. Port existing note-reading logic
4. Run against test suite
5. Fix failures without overfitting
