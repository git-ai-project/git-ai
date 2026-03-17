# Code Coverage Policy

## Overview

This project maintains a minimum code coverage threshold to ensure code quality and prevent regressions in test coverage.

## Current Threshold

**Minimum Coverage: 50%** (line coverage)

This threshold is enforced in CI via the GitHub Actions workflow at `.github/workflows/coverage.yml`.

## How the Threshold is Determined

The threshold is calculated as:
1. Measure current total code coverage percentage
2. Round down to the nearest multiple of 5
3. Set this as the minimum threshold

**Example:** If current coverage is 54.10%, the threshold is set to 50%.

## CI Enforcement

Pull requests and pushes to main will fail if:
- Code coverage falls below the configured threshold (50%)
- This is enforced using `cargo llvm-cov --fail-under-lines` in CI

Coverage reports are always generated and uploaded as artifacts, even if the threshold check fails.

## Checking Coverage Locally

### Quick Summary
```bash
task coverage
```

### HTML Report (Interactive)
```bash
task coverage:html
```

### LCOV Report (for tools/IDEs)
```bash
task coverage:lcov
```

### Check Against Threshold
```bash
task coverage:check
```

## Updating the Threshold

When code coverage improves significantly, you can update the threshold:

### Automatic Update
```bash
./scripts/update-coverage-threshold.sh
```

This script will:
1. Run coverage analysis
2. Calculate the new threshold (current coverage rounded down to nearest 5)
3. Update `.github/workflows/coverage.yml`

### Manual Update
Edit `.github/workflows/coverage.yml` and update the `COVERAGE_THRESHOLD` environment variable.

## Exclusions

The following are excluded from coverage calculation:
- Test files (`tests/**`)
- Benchmarks (`benches/**`)
- Examples (`examples/**`)

This ensures coverage metrics focus on production code quality.

## Coverage Reports

Coverage reports are generated on every CI run and available as artifacts:
- **HTML Report**: Interactive browsable report showing line-by-line coverage
- **LCOV Report**: Machine-readable format for IDE integration

Artifacts are retained for 30 days.

## Best Practices

1. **Write tests for new code**: Aim to maintain or improve coverage with each PR
2. **Review coverage reports**: Check which lines aren't covered and consider adding tests
3. **Don't game the metrics**: Focus on meaningful test coverage, not just hitting numbers
4. **Update threshold periodically**: As coverage improves, update the threshold to lock in gains
