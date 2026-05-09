#!/bin/bash
set -e

# Benchmark script for rebase v2 vs v3
# Creates a repo with N commits, injects AI notes, and measures rebase time

COMMIT_COUNT=${1:-50}
ITERATIONS=${2:-5}

echo "=== Rebase Performance Benchmark ==="
echo "Commits: $COMMIT_COUNT"
echo "Iterations: $ITERATIONS"
echo ""

# Build git-ai in release mode
echo "Building git-ai..."
cargo build --release >/dev/null 2>&1
BINARY_PATH=$(pwd)/target/release/git-ai

# Create test repo
BENCH_DIR=$(mktemp -d)
echo "Test repo: $BENCH_DIR"
cd "$BENCH_DIR"

git init
git config user.email "bench@test.com"
git config user.name "Benchmark"

# Disable gc during benchmark
git config gc.auto 0

echo "Creating base commit..."
echo "base content" > file.txt
git add file.txt
git commit -m "Initial commit"

# Get initial commit for branch point
BASE_SHA=$(git rev-parse HEAD)

# Create main branch with human commits
echo "Creating main branch with $COMMIT_COUNT commits..."
git branch -M main

for i in $(seq 1 $COMMIT_COUNT); do
    echo "main line $i" >> main.txt
    git add main.txt
    git commit -m "Main commit $i" >/dev/null
done

MAIN_TIP=$(git rev-parse HEAD)

# Create feature branch with AI commits
echo "Creating feature branch with $COMMIT_COUNT AI commits..."
git checkout -b feature "$BASE_SHA" >/dev/null

for i in $(seq 1 $COMMIT_COUNT); do
    echo "ai line $i" >> feature.txt

    git add feature.txt
    git commit -m "Feature commit $i" >/dev/null

    # Inject AI note for this commit
    COMMIT_SHA=$(git rev-parse HEAD)
    NOTE=$(cat <<EOF
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.4.0",
  "base_commit_sha": "$COMMIT_SHA",
  "attestations": [
    {
      "file_path": "feature.txt",
      "entries": [
        {
          "hash": "test-prompt-$i",
          "line_ranges": [{"Range": [$i, $i]}]
        }
      ]
    }
  ],
  "metadata": {
    "base_commit_sha": "$COMMIT_SHA",
    "prompts": {
      "test-prompt-$i": {
        "agent_id": {"tool": "test", "id": "bench-$i", "model": "test"},
        "human_author": "Test",
        "messages": [],
        "total_additions": 1,
        "total_deletions": 0,
        "accepted_lines": 1,
        "overriden_lines": 0
      }
    },
    "humans": {},
    "sessions": {}
  }
}
EOF
)
    git notes --ref=ai add -f -m "$NOTE" "$COMMIT_SHA" 2>/dev/null
done

FEATURE_TIP=$(git rev-parse HEAD)

echo "Setup complete: $COMMIT_COUNT commits on main, $COMMIT_COUNT AI commits on feature"
echo ""

# Benchmark V2
echo "=== Testing V2 (rebase_v3=false) ==="
V2_TIMES=()
V2_TOTAL=0

for i in $(seq 1 $ITERATIONS); do
    # Reset to feature branch
    git checkout -f feature >/dev/null 2>&1
    git reset --hard "$FEATURE_TIP" >/dev/null 2>&1

    # Clean up any leftover rebase state
    rm -rf .git/rebase-merge .git/rebase-apply 2>/dev/null || true

    # Measure rebase time with v2
    START=$(date +%s%N)

    GIT_AI_REBASE_V3=false \
    GIT_AI_DEBUG=0 \
    PATH="$(dirname $BINARY_PATH):$PATH" \
        git rebase main >/dev/null 2>&1 || {
            echo "  Run $i: FAILED (conflict or error)"
            git rebase --abort 2>/dev/null || true
            continue
        }

    END=$(date +%s%N)

    DURATION=$(( (END - START) / 1000000 ))  # Convert to ms
    V2_TIMES+=($DURATION)
    V2_TOTAL=$((V2_TOTAL + DURATION))
    echo "  Run $i: ${DURATION}ms"

    # Verify notes exist on rebased commits
    REBASED_SHA=$(git rev-parse HEAD)
    if ! git notes --ref=ai show "$REBASED_SHA" >/dev/null 2>&1; then
        echo "    WARNING: No note found on rebased commit (v2 data loss?)"
    fi
done

if [ ${#V2_TIMES[@]} -eq 0 ]; then
    echo "ERROR: All v2 runs failed"
    exit 1
fi

V2_AVG=$((V2_TOTAL / ${#V2_TIMES[@]}))
echo "  Average: ${V2_AVG}ms (${#V2_TIMES[@]} successful runs)"
echo ""

# Benchmark V3
echo "=== Testing V3 (rebase_v3=true) ==="
V3_TIMES=()
V3_TOTAL=0

for i in $(seq 1 $ITERATIONS); do
    # Reset to feature branch
    git checkout -f feature >/dev/null 2>&1
    git reset --hard "$FEATURE_TIP" >/dev/null 2>&1

    # Clean up any leftover rebase state
    rm -rf .git/rebase-merge .git/rebase-apply 2>/dev/null || true

    # Measure rebase time with v3
    START=$(date +%s%N)

    GIT_AI_REBASE_V3=true \
    GIT_AI_DEBUG=0 \
    PATH="$(dirname $BINARY_PATH):$PATH" \
        git rebase main >/dev/null 2>&1 || {
            echo "  Run $i: FAILED (conflict or error)"
            git rebase --abort 2>/dev/null || true
            continue
        }

    END=$(date +%s%N)

    DURATION=$(( (END - START) / 1000000 ))  # Convert to ms
    V3_TIMES+=($DURATION)
    V3_TOTAL=$((V3_TOTAL + DURATION))
    echo "  Run $i: ${DURATION}ms"

    # Verify notes exist on rebased commits
    REBASED_SHA=$(git rev-parse HEAD)
    if ! git notes --ref=ai show "$REBASED_SHA" >/dev/null 2>&1; then
        echo "    WARNING: No note found on rebased commit"
    fi
done

if [ ${#V3_TIMES[@]} -eq 0 ]; then
    echo "ERROR: All v3 runs failed"
    exit 1
fi

V3_AVG=$((V3_TOTAL / ${#V3_TIMES[@]}))
echo "  Average: ${V3_AVG}ms (${#V3_TIMES[@]} successful runs)"
echo ""

# Calculate results
echo "=== Results ==="
echo "V2 average: ${V2_AVG}ms"
echo "V3 average: ${V3_AVG}ms"

if [ $V2_AVG -gt 0 ]; then
    DIFF=$((V3_AVG - V2_AVG))
    PERCENT=$(( (DIFF * 100) / V2_AVG ))

    if [ $PERCENT -lt 0 ]; then
        SPEEDUP=$(( -PERCENT ))
        echo "Speedup: ${SPEEDUP}% faster with v3 ✅"
    else
        echo "Overhead: ${PERCENT}% slower with v3"

        if [ $PERCENT -gt 50 ]; then
            echo "❌ FAIL: V3 overhead exceeds 50% requirement"
            echo ""
            echo "Details:"
            echo "  Required: <50% overhead"
            echo "  Actual: ${PERCENT}% overhead"
            exit 1
        else
            echo "✅ PASS: V3 overhead within 50% requirement"
        fi
    fi
else
    echo "Cannot calculate percentage (v2 avg is 0)"
fi

echo ""
echo "=== Cleanup ==="
cd /
rm -rf "$BENCH_DIR"
echo "Removed $BENCH_DIR"
echo ""
echo "✅ Benchmark complete"
