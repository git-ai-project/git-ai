# Corehooks Implementation Report (Fixes 1-6)

Date: 2026-02-15
Branch: `feat/corehooks`

## Scope completed

Implemented all six planned fixes:

1. Native hook trampoline runtime (`hook-trampoline`) and early dispatch path in `main`.
2. Passthrough hooks moved to chain-only fast path (no internal git-ai hook dispatch).
3. Trampoline pre-dispatch filtering for `reference-transaction`.
4. Command-aware `reference-transaction` gating via `GIT_REFLOG_ACTION` action classification.
5. Shared hook-fast runtime path (`run_core_hook_best_effort`) used by both `hook` and trampoline paths.
6. Reduced hot-path git calls via per-invocation cache + cheap-first gating in `post-commit` and `reference-transaction`.

## Key code changes

- Added new trampoline module:
  - `/Users/svarlamov/projects/git-ai-c/src/commands/core_hook_trampoline.rs`
- Updated command wiring:
  - `/Users/svarlamov/projects/git-ai-c/src/main.rs`
  - `/Users/svarlamov/projects/git-ai-c/src/commands/git_ai_handlers.rs`
  - `/Users/svarlamov/projects/git-ai-c/src/commands/mod.rs`
- Refactored core hook runtime and script generation:
  - `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs`
- Updated install/up-to-date detection for new managed script formats:
  - `/Users/svarlamov/projects/git-ai-c/src/commands/install_hooks.rs`

## Compatibility validation

All required suites passed after implementation:

- `cargo test --test hook_contract_all_hooks`
- `cargo test --test hook_ecosystem_pre_commit`
- `cargo test --test hook_ecosystem_husky`
- `cargo test --test hook_ecosystem_lefthook`
- `cargo test --test rebase`
- `cargo test --test cherry_pick`
- `cargo test --test reset`
- `cargo test --test stash_attribution`

Plus focused unit checks:

- trampoline prefilter tests
- ref-action classification tests
- install-hooks script up-to-date tests

## Performance validation

### Workload benchmark (release build)

Method: 20x loop of `add + commit` in fresh temp repos, with isolated global git config
(`GIT_CONFIG_GLOBAL=$HOME/.gitconfig`, `GIT_CONFIG_NOSYSTEM=1`).

- Wrapper mode (git wrapper, no corehooks):
  - Runs (ms): `2779`, `2761`, `2879`
  - Median: `2779 ms`
- Corehooks mode (real git + installed corehooks):
  - Runs (ms): `6519`, `6243`, `6669`
  - Median: `6519 ms`

Corehooks vs wrapper ratio (fresh session):

- `6519 / 2779 = 2.346x` (134.6% slower)

### Warm-state benchmark (same repo/session)

Method: 40 sequential commits in one repo, then compare average per-commit times.

- Wrapper:
  - first 10 commits avg: `145.1 ms`
  - last 30 commits avg: `146.2 ms`
- Corehooks:
  - first 10 commits avg: `262.5 ms` (commit #1 was `867 ms`)
  - last 30 commits avg: `195.1 ms`

Corehooks vs wrapper ratio (steady-state, last 30 commits):

- `195.1 / 146.2 = 1.334x` (33.4% slower)

### Hook timing (single `add+commit`, trace2, corehooks)

Observed hook child durations (ms, first traced add+commit in fresh session):

- `post-index-change`: `327.121`, `4.574` (median `165.848`)
- `reference-transaction`: `323.334`, `3.456`, `2.990`, `2.801`, `2.649`, `2.601`, `2.486` (median `2.801`)
- `pre-commit`: `393.613`
- `post-commit`: `363.673`

### Process counts (single `add+commit`)

- Wrapper mode:
  - `start` events: `37`
  - hook child starts: `0`
- Corehooks mode:
  - `start` events: `39`
  - hook child starts: `13`

## Target status

Target from plan: corehooks should be <= `1.15x` wrapper.

Current measured status:

- fresh-session: `2.346x`
- steady-state: `1.334x`

Result: implementation complete, compatibility preserved, target not yet met.

## Remaining bottleneck summary

Largest remaining costs in corehooks path are high per-invocation hook runtime on commit path callbacks:

- `pre-commit` (~85ms)
- `post-commit` (~84ms)
- one expensive `reference-transaction` callback (~84ms)

These dominate the remaining gap after fixes 1-6.

## Follow-up optimization round (post fixes 1-6)

Date: 2026-02-15

Implemented additional high-impact optimizations focused on process fan-out and hook no-op callbacks:

- Hook repository resolution fast path from hook env (`GIT_DIR` / `GIT_WORK_TREE`) before `rev-parse`.
- Hook cache improvements:
  - reflog subject from `.git/logs/HEAD` (fallback to git command),
  - faster `HEAD` resolution (`rev-parse HEAD`),
  - post-commit parent/reflog reuse.
- Removed pre/post-commit state-file churn for `pending_commit_base_head`.
- Internal git subprocess hardening:
  - force `-c core.hooksPath=/dev/null` for internal `exec_git*` calls to prevent hook fan-out from git-ai’s own git commands.
- Added trampoline chain-skip mode (`GITAI_TRAMPOLINE_SKIP_CHAIN`) and script-level prefilters for hot hooks:
  - `reference-transaction` no-op fast paths,
  - `post-index-change` fast path.
- Added `pre-commit` launcher prefilter:
  - skip internal pre-commit dispatch when `.git/ai/working_logs` is absent/empty (still chains user hooks).
- Reduced shell shim overhead in passthrough/prefilter scripts while preserving chaining and failure propagation semantics.

### Follow-up performance validation (release build)

#### Realistic no-AI loop (20x `add+commit`, 5 runs)

- Wrapper runs (ms): `2577.47`, `2446.97`, `2416.86`, `2446.39`, `2553.55`
- Corehooks runs (ms): `2713.99`, `2686.92`, `2541.70`, `2507.14`, `2509.81`
- Medians:
  - Wrapper: `2446.97 ms`
  - Corehooks: `2541.70 ms`
- Ratio:
  - `2541.70 / 2446.97 = 1.039x`

#### AI-active loop (10x `checkpoint mock_ai` + `add+commit`, 3 runs)

- Wrapper runs (ms): `2271.50`, `2251.68`, `2373.54`
- Corehooks runs (ms): `2523.11`, `2430.59`, `2540.18`
- Medians:
  - Wrapper: `2271.50 ms`
  - Corehooks: `2523.11 ms`
- Ratio:
  - `2523.11 / 2271.50 = 1.111x`

### Per-hook no-op callback targets (steady-state microbench, warm tail)

- `reference-transaction` irrelevant callback:
  - average: `3.382 ms`
  - median: `3.345 ms`
- `post-index-change` callback:
  - average: `3.047 ms`
  - median: `2.961 ms`

### Updated target status

Target from plan: corehooks should be <= `1.15x` wrapper.

Measured after follow-up round:

- no-AI commit workload: `1.039x`
- AI-active commit workload: `1.111x`

Result: target met while preserving hook contract and rewrite/authorship test suites.
