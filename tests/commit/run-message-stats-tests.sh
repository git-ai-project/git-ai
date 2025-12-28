#!/usr/bin/env bash

# Integration test script for commit message stats feature
# Tests scenarios:
# 1. Commits without AI code should not modify messages
# 2. Commits with AI code should add stats
# 3. Mixed code should show correct percentages
# 4. Delete-only commits should show "(no additions)"
# 5. Different output formats (text/markdown)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "=========================================="
echo "Commit Message Stats Integration Tests"
echo "=========================================="
echo ""

# Check if compiled
if [ ! -f "$PROJECT_ROOT/target/debug/git-ai" ]; then
    echo "❌ git-ai binary not found, please run: cargo build"
    exit 1
fi

# Temporary test directory
TEST_DIR="/tmp/git-ai-commit-stats-test-$$"
cleanup() {
    if [ -z "$NO_CLEANUP" ]; then
        rm -rf "$TEST_DIR"
        echo "🧹 Cleaned up test directory"
    else
        echo "⚠️  Test directory preserved: $TEST_DIR"
    fi
}
trap cleanup EXIT

mkdir -p "$TEST_DIR"
cd "$TEST_DIR"

# Setup test environment
export GIT_AI="$PROJECT_ROOT/target/debug/git-ai"
export GIT_AI_COMMIT_MESSAGE_STATS=true
export PATH="$PROJECT_ROOT/target/debug:$PATH"

echo "Test directory: $TEST_DIR"
echo "GIT_AI: $GIT_AI"
echo ""

# Initialize git repo
init_repo() {
    git init
    git config user.name "Test User"
    git config user.email "test@example.com"
    git-ai config set --add feature_flags.commit_message_stats true
}

# Test 1: Commit without AI code
echo "Test 1: Commit without AI code"
echo "----------------------------------------"
init_repo
echo "initial" > init.txt
git add init.txt
GIT_AI=git git commit -m "Initial commit" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "Stats:"; then
    echo "❌ FAIL: Should not add stats"
    echo "   Message: $MSG"
    exit 1
else
    echo "✅ PASS: No stats added"
fi
echo ""

# Test 2: Pure AI code
echo "Test 2: Pure AI code"
echo "----------------------------------------"
echo "// AI generated" > ai.rs
git add ai.rs
git-ai checkpoint mock_ai ai.rs 2>&1 | grep -q "changed 1 file" || echo "⚠️  Checkpoint warning"
GIT_AI=git git commit -m "Add AI code" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "100% ai"; then
    echo "✅ PASS: Correctly shows 100% AI"
else
    echo "❌ FAIL: Should show 100% AI"
    echo "   Message: $MSG"
    exit 1
fi
echo ""

# Test 3: Mixed code (AI + human)
echo "Test 3: Mixed code (50% human, 50% AI)"
echo "----------------------------------------"
# Create file with AI line, then add human line
echo "// AI line" > mixed.rs
git add mixed.rs
git-ai checkpoint mock_ai mixed.rs 2>&1 | grep -q "changed 1 file" || true
echo "// human line" >> mixed.rs
git add mixed.rs
GIT_AI=git git commit -m "Mixed code" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "50%"; then
    echo "✅ PASS: Correctly shows 50% mixed"
    echo "   Message: $MSG"
else
    echo "❌ FAIL: Should show 50%"
    echo "   Message: $MSG"
    exit 1
fi
echo ""

# Test 4: Delete-only commit
echo "Test 4: Delete-only commit"
echo "----------------------------------------"
git rm mixed.rs
GIT_AI=git git commit -m "Remove file" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "(no additions)"; then
    echo "✅ PASS: Correctly shows (no additions)"
else
    echo "❌ FAIL: Should show (no additions)"
    echo "   Message: $MSG"
    exit 1
fi
echo ""

# Test 5: Markdown format
echo "Test 5: Markdown format"
echo "----------------------------------------"
git config ai.commit-message-stats.format markdown
echo "// AI code" > markdown.rs
git add markdown.rs
git-ai checkpoint mock_ai markdown.rs 2>&1 | grep -q "changed 1 file" || true
GIT_AI=git git commit -m "Markdown test" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "🧠" && echo "$MSG" | grep -q "🤖"; then
    echo "✅ PASS: Correctly shows Markdown format"
else
    echo "❌ FAIL: Should show Markdown format"
    echo "   Message: $MSG"
    exit 1
fi
echo ""

# Test 6: No AI code + Markdown format
echo "Test 6: No AI code + Markdown format"
echo "----------------------------------------"
echo "human only" > human.rs
git add human.rs
GIT_AI=git git commit -m "Human only" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "Stats:" || echo "$MSG" | grep -q "🧠"; then
    echo "❌ FAIL: Should not add stats"
    echo "   Message: $MSG"
    exit 1
else
    echo "✅ PASS: No stats added"
fi
echo ""

# Test 7: Disabled feature
echo "Test 7: Disabled feature"
echo "----------------------------------------"
git-ai config set --add feature_flags.commit_message_stats false
echo "test" > test.txt
git add test.txt
GIT_AI=git git commit -m "Disabled test" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "Stats:"; then
    echo "❌ FAIL: Feature disabled, should not add stats"
    echo "   Message: $MSG"
    exit 1
else
    echo "✅ PASS: Feature disabled correctly"
fi
echo ""

# Test 8: Custom template
echo "Test 8: Custom template"
echo "----------------------------------------"
git-ai config set --add feature_flags.commit_message_stats true
git config ai.commit-message-stats.template "📝 {original_message}\\n\\n📊 {stats}"
echo "// AI" > custom.rs
git add custom.rs
git-ai checkpoint mock_ai custom.rs 2>&1 | grep -q "changed 1 file" || true
GIT_AI=git git commit -m "Custom template" 2>&1 | grep -v "^\[" || true

MSG=$(git log -1 --format='%B')
if echo "$MSG" | grep -q "📝" && echo "$MSG" | grep -q "📊"; then
    echo "✅ PASS: Custom template applied"
else
    echo "❌ FAIL: Custom template not applied"
    echo "   Message: $MSG"
    exit 1
fi
echo ""

# Test 9: Git Notes sync (SHA change)
echo "Test 9: Git Notes sync (SHA change)"
echo "----------------------------------------"
git config ai.commit-message-stats.format text
git config ai.commit-message-stats.template "{original_message}\\n\\n{stats}"
echo "// AI 2" > note.rs
git add note.rs
git-ai checkpoint mock_ai note.rs 2>&1 | grep -q "changed 1 file" || true

SHA_BEFORE=$(git rev-parse HEAD)

GIT_AI=git git commit -m "Notes test" 2>&1 | grep -v "^\[" || true

SHA_AFTER=$(git rev-parse HEAD)

if [ "$SHA_BEFORE" != "$SHA_AFTER" ]; then
    echo "✅ PASS: SHA correctly changed (needs Git Notes sync)"

    if git notes --ref=ai show &>/dev/null; then
        echo "✅ PASS: Git Notes exists"
    else
        echo "⚠️  WARNING: Git Notes not found"
    fi
else
    echo "❌ FAIL: SHA should change"
    exit 1
fi
echo ""

# Test 10: View full history
echo "Test 10: Full history"
echo "----------------------------------------"
echo "All commits:"
git log --oneline
echo ""
echo "Detailed messages (last 5):"
git log --format='=== %s ===%n%B%n' -5
echo ""

echo "=========================================="
echo "All tests passed! ✅"
echo "=========================================="
