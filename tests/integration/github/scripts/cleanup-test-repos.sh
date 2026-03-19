#!/usr/bin/env bash

# This script cleans up GitHub repositories created by integration tests.
# It searches for repositories matching the pattern 'git-ai-test-*' and deletes them.

set -e

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
echo ""

# Get authenticated user
GITHUB_USER=$(gh api user --jq '.login')
echo "👤 Authenticated as: $GITHUB_USER"
echo ""

# Find all test repositories
echo "🔍 Searching for test repositories (git-ai-test-*)..."
echo ""

# Get list of repositories matching the pattern
REPOS=$(gh repo list "$GITHUB_USER" --json name --jq '.[] | select(.name | startswith("git-ai-test-")) | .name')

if [ -z "$REPOS" ]; then
    echo "✅ No test repositories found to clean up"
    exit 0
fi

# Count repositories
REPO_COUNT=$(echo "$REPOS" | wc -l)

echo "Found $REPO_COUNT test repositories:"
echo ""
echo "$REPOS" | while read -r repo; do
    echo "  - $GITHUB_USER/$repo"
done
echo ""

# Ask for confirmation
read -p "⚠️  Delete all $REPO_COUNT repositories? [y/N] " -n 1 -r
echo ""

if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "❌ Cleanup cancelled"
    exit 0
fi

echo ""
echo "🗑️  Deleting repositories..."
echo ""

# Delete each repository
DELETED=0
FAILED=0

echo "$REPOS" | while read -r repo; do
    FULL_REPO="$GITHUB_USER/$repo"
    echo -n "  Deleting $FULL_REPO... "

    if gh repo delete "$FULL_REPO" --yes 2>/dev/null; then
        echo "✅"
        DELETED=$((DELETED + 1))
    else
        echo "❌"
        FAILED=$((FAILED + 1))
    fi
done

echo ""
echo "✅ Cleanup complete"
echo "   Deleted: $REPO_COUNT repositories"

if [ $FAILED -gt 0 ]; then
    echo "⚠️  Failed: $FAILED repositories"
    exit 1
fi
