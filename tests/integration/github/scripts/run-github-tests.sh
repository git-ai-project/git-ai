#!/usr/bin/env bash

# This script is used to run the GitHub integration tests.
# These tests create actual GitHub repositories and PRs, so are not included in the default test suite.

# Run with:
# ./run-github-tests.sh
#
# Or with --no-cleanup to leave the test repositories in place for manual inspection:
# ./run-github-tests.sh --no-cleanup 

set -euo pipefail

# Parse arguments
NO_CLEANUP=0
TEST_ARGS=()

for arg in "$@"; do
    if [ "$arg" = "--no-cleanup" ]; then
        NO_CLEANUP=1
    else
        TEST_ARGS+=("$arg")
    fi
done

echo "🔍 Checking GitHub CLI availability..."
if ! command -v gh &> /dev/null; then
    echo "❌ GitHub CLI (gh) is not installed"
    echo "   Install from: https://cli.github.com/"
    exit 1
fi

if ! gh auth status &> /dev/null; then
    echo "❌ GitHub CLI is not authenticated"
    echo "   Run: gh auth login"
    exit 1
fi

echo "✅ GitHub CLI is available and authenticated"

if [ $NO_CLEANUP -eq 1 ]; then
    echo "⚠️  Cleanup disabled - test repositories will NOT be deleted"
    export GIT_AI_TEST_NO_CLEANUP=1
fi

echo ""
echo "🚀 Running GitHub integration tests..."
echo ""

cargo test --test github_integration -- --ignored --nocapture "${TEST_ARGS[@]}"
