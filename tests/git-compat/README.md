# Git Compatibility Tests

This directory contains tools for testing git-ai compatibility with the official Git test suite.

## Overview

Git has an extensive test suite in the `t/` directory of the git repository. These tests use a
[TAP-based framework](https://github.com/git/git/tree/master/t#readme) and can be run against
any git installation by setting the `GIT_TEST_INSTALLED` environment variable.

We run a subset of these tests in CI to ensure that git-ai doesn't break core git functionality
when used as a git wrapper.

## CI Workflow

The GitHub Actions workflow (`.github/workflows/git-compat-tests.yml`) runs automatically on:
- Pull requests to main
- Pushes to main  
- Manual trigger via workflow_dispatch

### What the CI does:

1. **Builds git-ai** in release mode
2. **Clones the official git repository** (pinned to v2.47.1 for reproducibility)
3. **Builds standard git** for reference baseline
4. **Runs a subset of git tests** with standard git (to establish baseline failures)
5. **Runs the same tests with git-ai** enabled via GIT_TEST_INSTALLED
6. **Compares results** to detect regressions (tests that fail only with git-ai)

### Test Selection

The test subset (~150 tests) covers core git functionality:
- Basic operations (init, add, commit, status)
- Branching and merging
- Rebase and cherry-pick
- Stash operations
- Diff and log
- Remote operations (fetch, push, pull, clone)
- Hooks
- Notes (important for git-ai!)
- Reset and checkout
- And more...

## Local Development

### Running the full comparison locally

The `run.py` script compares git-ai against standard git on your local machine:

```bash
# First, build git-ai
cargo build --release

# Set up gitwrap symlinks
mkdir -p ~/.git-ai-test/gitwrap/bin
ln -sf $(pwd)/target/release/git-ai ~/.git-ai-test/gitwrap/bin/git
ln -sf $(pwd)/target/release/git-ai ~/.git-ai-test/gitwrap/bin/git-ai

# Clone the git repository (if not already done)
git clone https://github.com/git/git.git ~/projects/git

# Run the comparison (edit paths in run.py first)
python3 tests/git-compat/run.py
```

### Running a single test

To debug a specific test:

```bash
cd ~/projects/git/t
GIT_TEST_INSTALLED=~/.git-ai-test/gitwrap/bin ./t3301-notes.sh -v
```

The `-v` flag enables verbose output. You can also use `--run=<n>` to run specific subtests:

```bash
GIT_TEST_INSTALLED=~/.git-ai-test/gitwrap/bin ./t3301-notes.sh -v --run=1-5
```

## Whitelist

Some tests are whitelisted in `whitelist.csv` because they fail due to known incompatibilities
(e.g., non-UTF-8 character handling). These failures are excluded from regression detection.

Format:
```csv
file,test,rationale
"t4201-shortlog.sh","1,2,3,4,5,6,7,8,9,13","Errors related to non-UTF-8 chars"
```

## Files

- `compare_results.py` - CI script to compare prove output and detect regressions
- `run.py` - Local development script for full comparison
- `whitelist.csv` - Known test failures to ignore
- `README.md` - This file

## References

- [Git Test Framework Documentation](https://github.com/git/git/tree/master/t#readme)
- [GitHub Issue #436](https://github.com/git-ai-project/git-ai/issues/436)
