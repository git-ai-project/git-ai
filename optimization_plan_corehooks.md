# Optimization Plan: `feat/corehooks` Performance

Date: 2026-02-15

## Status

- Phase 1: completed
- Phase 2: completed
- Validation snapshot (post-implementation):
  - Wrapper fast-path regression vs `main`: largely removed; `old` -> `new` improved substantially on `rev-parse` and `status`.
  - Core-hooks vs wrapper (realistic multi-commit workload): improved from ~3.35x to ~2.80x.
  - No-op hook overhead:
    - `post-index-change`: ~17.7ms -> ~6.0ms per call
    - `reference-transaction` (irrelevant refs): ~17.6ms -> ~7.3ms per call

## Performance targets (exit criteria)

1. Wrapper mode (`git` shim) fast commands:
   - `rev-parse --is-inside-work-tree`: <= +3% vs `main`
   - `status --porcelain`: <= +5% vs `main`
2. Core-hooks mode:
   - realistic commit workload: <= 1.15x wrapper mode on same branch
3. Hook execution overhead:
   - no-op hook invocation (`post-index-change`): <= 8ms average
   - no-op/irrelevant `reference-transaction`: <= 10ms average

## Phase 0: Lock in measurement harness (1 day)

### Changes
- Add a repeatable benchmark script under `scripts/perf/` to run:
  - wrapper vs main (`rev-parse`, `status`, `diff`)
  - wrapper vs core-hooks (`add/reset`, multi-commit loop)
  - trace summary (`GIT_TRACE2_EVENT`) process counts
- Add a CI optional/manual perf job (non-blocking at first) that stores artifacts.
- Add per-hook perf logging in `core_hooks` (guarded by `GIT_AI_DEBUG_PERFORMANCE>=2`).

### Files
- `/Users/svarlamov/projects/git-ai-c/scripts/perf/*` (new)
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs`
- `/Users/svarlamov/projects/git-ai-c/.github/workflows/*` (optional perf job)

### Expected impact
- No runtime impact; improves confidence and prevents regressions from reappearing.

## Phase 1: High-ROI startup/path fixes (low risk, 1-2 days)

## 1. Hook fast-path in `handle_git_ai`

### Changes
- In `/Users/svarlamov/projects/git-ai-c/src/commands/git_ai_handlers.rs`, dispatch `"hook"` before:
  - `find_repository_in_path()`
  - `Config::get()`
  - allowlist checks
  - DB warmup match
- This removes one full preamble from every hook invocation.

### Expected impact
- Significant core-hooks win (removes duplicated repo discovery + remote checks per hook process).

## 2. Lazy remote lookup in allowlist checks

### Changes
- In `/Users/svarlamov/projects/git-ai-c/src/config.rs`, change `is_allowed_repository()` to:
  - return `true` immediately when both `allow_repositories` and `exclude_repositories` are empty
  - only call `repo.remotes_with_urls()` when a non-empty allow/exclude list needs it

### Expected impact
- Removes one `git remote -v` subprocess from most wrapper and hook invocations.

## 3. Restore single-call non-bare fast path in repository discovery

### Changes
- In `/Users/svarlamov/projects/git-ai-c/src/git/repository.rs` (`find_repository`):
  - fast path: try `rev-parse --git-dir --show-toplevel` first (one call)
  - fallback path only if needed: bare-aware flow (`--is-bare-repository --git-dir`, etc.)
- Keep bare repo behavior from `506bafdd`.

### Expected impact
- Primary fix for wrapper regression vs `main` on fast commands.

## 4. Validation gates after Phase 1

### Required checks
- Run wrapper benchmarks from Phase 0 harness.
- Confirm wrapper fast commands are back within targets.
- Confirm no regressions in:
  - bare-repo tests
  - subdirectory `-C` variants
  - hook ecosystem tests

## Phase 2: Reduce core-hooks invocation volume/cost (medium risk, 2-4 days)

## 5. Stop spawning `git-ai hook` for passthrough-only hooks

### Changes
- In `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs` script generation:
  - Generate a lighter script variant for passthrough-only hooks:
    - `applypatch-msg`, `pre-applypatch`, `post-applypatch`, `pre-merge-commit`, `prepare-commit-msg`, `commit-msg`, `pre-auto-gc`
  - Pass-through scripts should only chain previous/repo hook and never call `git-ai hook`.

### Expected impact
- Immediate commit-path improvement (removes extra process launches for `prepare-commit-msg` and `commit-msg`).

## 6. Add strict early-exit path in `reference-transaction`

### Changes
- In `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs` (`handle_reference_transaction`):
  - Parse stdin refs first.
  - If no relevant refs are touched, return before:
    - `load_core_hook_state()`
    - stash/reflog/remote operations
  - Avoid `save_core_hook_state()` when state is unchanged.

### Expected impact
- Large reduction in high-frequency transaction hook overhead.

## 7. Minimize state file churn

### Changes
- In core hook state helpers, only persist when values changed.
- Avoid read-modify-write sequences when no update is required.

### Expected impact
- Lower filesystem overhead under repeated hook firing.

## 8. Validation gates after Phase 2

### Required checks
- Core-hooks benchmark ratios from Phase 0 harness.
- `GIT_TRACE2_EVENT` comparison:
  - target: substantial reduction in `start` count for one `add+commit`.
- Regression suite:
  - rebase/cherry-pick/reset/stash attribution tests
  - ecosystem hook compatibility tests

## Phase 3: Structural optimization (higher effort, likely required for 1.15x target, 3-5 days)

## 9. Dedicated lightweight hook entry mode

### Changes
- Introduce a minimal entry path for hooks (e.g. `argv[0] == "git-ai-hook"`).
- Hook scripts call this entry mode instead of full `git-ai` CLI dispatch.
- Skip command router work not needed for hooks.

### Expected impact
- Additional per-hook startup reduction beyond Phase 1/2.

## 10. Optional feature-gated hook set

### Changes
- Add config/feature flag profiles:
  - `corehooks=minimal` (only required hooks)
  - `corehooks=full` (current behavior)
- Keep current behavior as opt-in for users needing full rewrite tracking.

### Expected impact
- Lets most users run lower-overhead mode while retaining advanced behavior.

## Implementation order (recommended)

1. Phase 0 harness
2. Phase 1 items 1-3
3. Measure and decide go/no-go for Phase 2
4. Phase 2 items 5-7
5. Re-measure; only pursue Phase 3 if target still missed

## Risk notes

1. `find_repository` fast-path changes must preserve bare-repo correctness and `-C` behavior.
2. Passthrough hook script split must preserve Husky/Lefthook/pre-commit chaining semantics.
3. `reference-transaction` early exits must not break reset/rebase/stash attribution updates.

## Done definition

1. Performance targets met on local harness and CI perf job.
2. Existing integration suites remain green.
3. No change in authorship correctness snapshots for representative rewrite workflows.
