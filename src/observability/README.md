# Observability

Performance measurement and debugging instrumentation.

## What it does

Provides lightweight performance timing that reports when operations exceed their budgets. Enabled by environment variables — zero overhead when disabled.

## Activation

- `GIT_AI_DEBUG_PERFORMANCE=1` — human-readable timing output to stderr
- `GIT_AI_DEBUG_PERFORMANCE=2` — JSON timing output (for automated collection)

## Key types

- `PerfTimer` — Drop-based timer that reports elapsed time on scope exit
- `perf_time!` macro — creates a named `PerfTimer` for the current scope

## Performance budgets

The module defines target latencies for critical operations:

| Operation | Budget |
|-----------|--------|
| Checkpoint | 3ms |
| Post-commit (daemon) | 1ms |
| Post-commit (sync) | 3ms |
| Blame (100 commits) | 6ms |

Budget violations are flagged in output to catch regressions early.

## Design

Uses `OnceLock` to cache the env var check at first access. The timer uses `std::time::Instant` with no allocations on the hot path when disabled.
