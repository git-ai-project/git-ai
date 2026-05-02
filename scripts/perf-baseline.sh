#!/bin/bash
set -e

# Subprocess Performance Baseline Collection Script
# Collects subprocess call patterns and timing for git-ai operations

PLATFORM=$(uname -s)
OUTPUT_DIR="${1:-./perf-baseline-results}"
GIT_AI_BIN="${GIT_AI_BIN:-git-ai}"

mkdir -p "$OUTPUT_DIR"

echo "=== Git-AI Performance Baseline Collection ==="
echo "Platform: $PLATFORM"
echo "Output: $OUTPUT_DIR"
echo "Git-AI: $GIT_AI_BIN"
echo ""

# Create test repository
TEST_DIR=$(mktemp -d)
cd "$TEST_DIR"
echo "Test repo: $TEST_DIR"

git init -q
git config user.email "test@test.com"
git config user.name "Test User"

# Enable instrumentation
export GIT_AI_INSTRUMENT_SUBPROCESSES=1
export GIT_AI_INSTRUMENT_JSON=1

# Helper to extract JSON from stderr
extract_json() {
    local output_file="$1"
    grep "^{" | head -1 > "$output_file" 2>/dev/null || echo "{}" > "$output_file"
}

echo "Running benchmarks..."

# Benchmark 1: git-ai status (empty, no checkpoints)
echo "  1/5: status (empty)"
echo "test line" > test.txt
git add test.txt
git commit -q -m "initial"
$GIT_AI_BIN status 2>&1 | extract_json "$OUTPUT_DIR/01-status-empty.json"

# Benchmark 2: git-ai checkpoint human
echo "  2/5: checkpoint human"
echo "line 2" >> test.txt
$GIT_AI_BIN checkpoint human test.txt 2>&1 | extract_json "$OUTPUT_DIR/02-checkpoint-human.json"

# Benchmark 3: git-ai checkpoint ai
echo "  3/5: checkpoint ai"
echo "line 3" >> test.txt
$GIT_AI_BIN checkpoint mock_ai test.txt 2>&1 | extract_json "$OUTPUT_DIR/03-checkpoint-ai.json"

# Benchmark 4: git commit (triggers post-commit hook)
echo "  4/5: commit with hooks"
git add test.txt
git commit -q -m "test commit" 2>&1 | extract_json "$OUTPUT_DIR/04-commit-hooks.json" || true

# Benchmark 5: git-ai status (with checkpoints)
echo "  5/5: status (with checkpoints)"
echo "line 4" >> test.txt
$GIT_AI_BIN checkpoint human test.txt 2>&1 >/dev/null
$GIT_AI_BIN status 2>&1 | extract_json "$OUTPUT_DIR/05-status-with-checkpoints.json"

# Benchmark 6: Large file operation
echo "  6/6: large commit (100 files)"
for i in $(seq 1 100); do
    echo "content $i" > "file$i.txt"
done
git add .
git commit -q -m "large commit" 2>&1 | extract_json "$OUTPUT_DIR/06-large-commit.json" || true

# Generate summary
cd "$OUTPUT_DIR"
echo ""
echo "=== Results Summary ==="
echo ""

for json_file in *.json; do
    if [ -f "$json_file" ] && [ -s "$json_file" ]; then
        total_calls=$(jq -r '.total_calls // 0' "$json_file" 2>/dev/null || echo "0")
        total_ms=$(jq -r '.total_duration_ms // 0' "$json_file" 2>/dev/null || echo "0")
        elapsed_ms=$(jq -r '.elapsed_ms // 0' "$json_file" 2>/dev/null || echo "0")

        if [ "$elapsed_ms" -gt 0 ]; then
            pct=$((total_ms * 100 / elapsed_ms))
        else
            pct=0
        fi

        printf "%-30s %3d calls  %6dms subprocess (%3d%% of %dms total)\n" \
            "$json_file" "$total_calls" "$total_ms" "$pct" "$elapsed_ms"
    fi
done

echo ""
echo "Detailed results saved to: $OUTPUT_DIR"
echo "Cleanup test repo: rm -rf $TEST_DIR"
