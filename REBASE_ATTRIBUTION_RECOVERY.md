# Rebase Attribution Loss: Comprehensive Test Coverage & Recovery Plan

## Problem Statement

Users report that git-ai **consistently drops AI authorship metadata during Git rebases and squashes**. After a rebase completes, Git Notes fail to migrate to newly generated commit SHAs, causing all AI-authored code to be permanently misclassified as unknown additions. This affects nearly 50% of AI-assisted commits in enterprise environments.

## Root Causes Identified

### 1. **Daemon Sync Timing Races**
- Rebase triggered before daemon completes background flush
- Working logs not available when rebase hook fires
- No synchronous fallback in all code paths

### 2. **External Tool Rebases**
- `git pull --rebase` bypasses certain hooks
- GitHub squash-merge doesn't trigger local hooks
- IDE rebases may run with wrong environment

### 3. **Interactive Rebase Complexity**
- Squash/fixup merge multiple commits → single SHA
- Notes from N commits must merge into 1
- Edit/reword/drop create complex commit mappings

### 4. **Working Log Issues**
- Detached HEAD checkpoints keyed incorrectly
- Working logs cleaned up/corrupted before rebase
- Base commit mismatch between checkpoint and rebase

### 5. **Conflict Resolution Gaps**
- Manual conflict resolution without checkpoints
- Multi-file conflicts lose attribution
- `git rebase --skip` drops commits unexpectedly

### 6. **Silent Failures**
- No pre-flight validation warns users
- No post-rebase verification detects loss
- No recovery mechanism after loss detected

## Test Coverage Added

### File: `tests/integration/rebase_production_failures.rs`

Comprehensive test suite covering all production failure modes:

#### ✅ **Daemon Sync Timing**
- `test_rebase_before_daemon_sync_completes` - Fast rebase before flush
- `test_rebase_during_working_log_flush` - Rebase mid-flush

#### ✅ **External Tool Rebases**
- `test_pull_rebase_preserves_attribution` - Common `git pull --rebase` flow
- `test_rebase_no_verify_warns_user` - `--no-verify` flag detection (ignored - needs implementation)

#### ✅ **Interactive Rebase**
- `test_interactive_rebase_squash_merges_notes` - Squash 3→1 commit
- `test_rebase_onto_different_base` - `--onto` complex rebase

#### ✅ **Working Log Edge Cases**
- `test_checkpoint_on_detached_head` - Detached HEAD checkpoint
- `test_rebase_with_missing_working_logs` - Graceful degradation (ignored - needs implementation)
- `test_working_log_base_commit_mismatch` - Wrong base commit key

#### ✅ **Multi-File Conflicts**
- `test_rebase_conflicts_on_multiple_ai_files` - 3 files conflict, manual resolution
- `test_rebase_skip_drops_commit_cleanly` - `--skip` preserves remaining notes

#### ✅ **Error Recovery**
- `test_rebase_abort_restores_original_notes` - `--abort` keeps original state
- `test_rebase_hook_failure_fails_loudly` - Hook failures don't silent-fail (ignored - needs implementation)

### File: `tests/integration/rebase_determinism.rs`

Cross-platform determinism tests (already completed):
- Tree hash equivalence (git-ai = native git)
- SHA determinism with frozen environment
- Line number mapping through rebase
- File deletion + recreation

## Implementation TODO

### Phase 1: Pre-Flight Validation ⏳ (Task #3)

**File**: `src/commands/hooks/rebase_hooks.rs`

```rust
fn validate_rebase_preconditions(repo: &Repository) -> Result<(), String> {
    // Check daemon sync status
    if !daemon_is_synced(repo)? {
        warn!("Background sync pending. AI attribution may be incomplete.");
        warn!("Run `git-ai sync --wait` before rebasing.");
    }
    
    // Check uncommitted checkpoints
    let pending = count_pending_checkpoints(repo)?;
    if pending > 0 {
        warn!("{} uncommitted AI checkpoints. Commit changes before rebasing.", pending);
    }
    
    // Check working logs exist
    let head_sha = repo.head_commit()?;
    if !working_logs_exist_for(&head_sha)? {
        warn!("No AI working logs found. Attribution may not survive rebase.");
    }
}
```

**Trigger**: Run in `rebase_hooks::pre_rebase()`

### Phase 2: Post-Rebase Verification ⏳ (Task #4)

**File**: `src/commands/hooks/rebase_hooks.rs`

```rust
fn verify_rebase_attribution(
    repo: &Repository,
    original_shas: &[String],
    rebased_shas: &[String]
) -> Result<()> {
    let mut missing_notes = vec![];
    
    for (orig_sha, new_sha) in original_shas.iter().zip(rebased_shas) {
        if let Some(_orig_note) = read_note(repo, orig_sha)? {
            if read_note(repo, new_sha)?.is_none() {
                missing_notes.push((orig_sha.clone(), new_sha.clone()));
            }
        }
    }
    
    if !missing_notes.is_empty() {
        eprintln!("[git-ai] ERROR: Authorship notes lost during rebase!");
        eprintln!("  {} commits missing AI attribution", missing_notes.len());
        for (orig, new) in &missing_notes {
            eprintln!("  {} → {}", &orig[..8], &new[..8]);
        }
        eprintln!("\nTo recover:");
        eprintln!("  git-ai rebase recover --from {} --to {}", 
                  original_shas[0], rebased_shas.last().unwrap());
        
        return Err(GitAiError::AttributionLoss);
    }
    
    Ok(())
}
```

**Trigger**: Run in `rebase_hooks::post_rebase()`

### Phase 3: Recovery Command ⏳ (Task #1)

**File**: `src/commands/rebase_recover.rs` (new)

```rust
pub fn recover_attribution(
    repo: &Repository,
    original_sha: &str,
    rebased_sha: &str
) -> Result<()> {
    // 1. Read working logs from original commits
    let original_commits = get_commit_range(repo, original_sha)?;
    let working_logs = original_commits
        .iter()
        .filter_map(|sha| read_working_log(repo, sha).ok())
        .collect::<Vec<_>>();
    
    // 2. Get rebased commits
    let rebased_commits = get_commit_range(repo, rebased_sha)?;
    
    // 3. Map original → rebased by diffing content
    let mapping = pair_commits_by_content_diff(
        repo,
        &original_commits,
        &rebased_commits
    )?;
    
    // 4. Reconstruct notes
    for (orig_sha, new_sha) in mapping {
        if let Some(working_log) = working_logs.get(&orig_sha) {
            let recovered_note = reconstruct_note_from_working_log(
                repo,
                new_sha,
                working_log
            )?;
            write_note(repo, new_sha, &recovered_note)?;
        }
    }
    
    Ok(())
}
```

**CLI**: `git-ai rebase recover --from <SHA> --to <SHA>`

### Phase 4: Graceful Degradation

For ignored tests that currently fail:

1. **Missing Working Logs**: Instead of failing, use heuristics:
   - Check git blame on original commit
   - Look for AI session markers in commit messages
   - Fall back to "unknown" attribution with clear warning

2. **Hook Failures**: Capture and display errors:
   - Don't let rebase complete silently
   - Show actionable error messages
   - Suggest recovery steps

3. **--no-verify Detection**: Post-rebase scan:
   - Check if hooks ran by looking for markers
   - If markers missing, warn user
   - Offer to run attribution recovery

## Testing Strategy

### Current Test Results

All non-ignored tests passing:
- ✅ Daemon sync races
- ✅ Pull rebase flows  
- ✅ Interactive rebase
- ✅ Detached HEAD
- ✅ Multi-file conflicts
- ✅ Rebase skip/abort

### Ignored Tests (Require Implementation)

- ⏳ `test_rebase_no_verify_warns_user` - Need warning system
- ⏳ `test_rebase_with_missing_working_logs` - Need graceful degradation
- ⏳ `test_rebase_hook_failure_fails_loudly` - Need robust error handling

### Test Execution

```bash
# Run all production failure tests
cargo test --test integration rebase_production_failures

# Run specific failure scenario
cargo test --test integration rebase_production_failures::test_rebase_before_daemon_sync_completes

# Include ignored tests (will fail until implemented)
cargo test --test integration rebase_production_failures -- --ignored
```

## Success Criteria

1. **Zero Silent Failures**: Every attribution loss scenario either:
   - Preserves attribution correctly, OR
   - Shows clear warning before loss occurs, OR
   - Provides recovery mechanism after detection

2. **User Visibility**: Users must see:
   - Pre-flight warnings when conditions risky
   - Post-rebase verification results
   - Clear recovery instructions if loss detected

3. **Enterprise Reliability**: 
   - Attribution survival rate >99% (currently ~50%)
   - All common rebase flows covered
   - Recovery possible for any detected loss

## Related Issues

- #1079: Original daemon race condition (fixed)
- Production reports: Enterprise attribution loss (~50% affected commits)
- GitHub squash-merge flows
- IDE rebase integration issues

## Next Steps

1. ✅ Complete production failure test suite (Task #2)
2. ⏳ Implement pre-flight validation (Task #3)
3. ⏳ Implement post-rebase verification (Task #4)
4. ⏳ Implement recovery command (Task #1)
5. ⏳ Run full test suite and validate (Task #5)
6. Update user documentation with best practices
7. Add telemetry to track attribution survival rates
