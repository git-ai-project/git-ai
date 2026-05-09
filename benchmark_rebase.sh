#!/bin/bash
set -e

# Benchmark script for rebase v2 vs v3
# Creates a repo with N commits, injects AI notes, and measures rebase time

COMMIT_COUNT=${1:-100}
ITERATIONS=${2:-5}

echo "=== Rebase Performance Benchmark ==="
echo "Commits: $COMMIT_COUNT"
echo "Iterations: $ITERATIONS"
echo ""

# Build git-ai
cargo build --release >/dev/null 2>&1

# Create test repo
BENCH_DIR=$(mktemp -d)
cd "$BENCH_DIR"
git init
git config user.email "bench@test.com"
git config user.name "Benchmark"

echo "Creating $COMMIT_COUNT commits with AI notes..."

# Base commit
echo "line 1" > file.txt
git add file.txt
git commit -m "Initial commit"

# Add commits with AI attribution
for i in $(seq 1 $COMMIT_COUNT); do
    echo "line $i" >> file.txt

    # Inject AI note for this commit
    git add file.txt
    git commit -m "Commit $i"

    # Create a fake AI note
    COMMIT_SHA=$(git rev-parse HEAD)
    NOTE=$(cat <<EOF
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.4.0",
  "base_commit_sha": "$COMMIT_SHA",
  "attestations": [
    {
      "file_path": "file.txt",
      "entries": [
        {
          "hash": "test-prompt-id",
          "line_ranges": [{"Range": [$i, $i]}]
        }
      ]
    }
  ],
  "metadata": {
    "base_commit_sha": "$COMMIT_SHA",
    "prompts": {
      "test-prompt-id": {
        "agent_id": {"tool": "test", "id": "bench", "model": "test"},
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
    git notes --ref=ai add -f -m "$NOTE" "$COMMIT_SHA"
done

echo "Created $COMMIT_COUNT commits"
echo ""

# Create a branch to rebase
git checkout -b feature
git checkout main

# Benchmark V2
echo "=== Testing V2 (rebase_v3=false) ==="
V2_TIMES=()
for i in $(seq 1 $ITERATIONS); do
    git checkout feature
    git reset --hard HEAD~$COMMIT_COUNT

    START=$(date +%s%N)
    GIT_AI_REBASE_V3=false git rebase main >/dev/null 2>&1
    END=$(date +%s%N)

    DURATION=$(( (END - START) / 1000000 ))  # Convert to ms
    V2_TIMES+=($DURATION)
    echo "  Run $i: ${DURATION}ms"
done

# Calculate V2 average
V2_SUM=0
for time in "${V2_TIMES[@]}"; do
    V2_SUM=$((V2_SUM + time))
done
V2_AVG=$((V2_SUM / ITERATIONS))
echo "  Average: ${V2_AVG}ms"
echo ""

# Benchmark V3
echo "=== Testing V3 (rebase_v3=true) ==="
V3_TIMES=()
for i in $(seq 1 $ITERATIONS); do
    git checkout feature
    git reset --hard HEAD~$COMMIT_COUNT

    START=$(date +%s%N)
    GIT_AI_REBASE_V3=true git rebase main >/dev/null 2>&1
    END=$(date +%s%N)

    DURATION=$(( (END - START) / 1000000 ))  # Convert to ms
    V3_TIMES+=($DURATION)
    echo "  Run $i: ${DURATION}ms"
done

# Calculate V3 average
V3_SUM=0
for time in "${V3_TIMES[@]}"; do
    V3_SUM=$((V3_SUM + time))
done
V3_AVG=$((V3_SUM / ITERATIONS))
echo "  Average: ${V3_AVG}ms"
echo ""

# Calculate overhead
OVERHEAD=$(( (V3_AVG - V2_AVG) * 100 / V2_AVG ))
echo "=== Results ==="
echo "V2 average: ${V2_AVG}ms"
echo "V3 average: ${V3_AVG}ms"
echo "Overhead: ${OVERHEAD}%"

if [ $OVERHEAD -gt 50 ]; then
    echo "❌ FAIL: V3 overhead exceeds 50% requirement"
    exit 1
else
    echo "✅ PASS: V3 overhead within acceptable range"
fi

# Cleanup
cd /
rm -rf "$BENCH_DIR"
