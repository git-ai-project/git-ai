#!/usr/bin/env bash
# Feed synthetic checkpoint data through the pipeline and verify authorship notes.
# Usage: test-synthetic-checkpoint.sh <agent> [repo-dir]
set -euo pipefail

AGENT="${1:?Usage: $0 <agent> [repo-dir]}"
REPO_DIR="${2:-/tmp/test-repo}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/test-results}"
mkdir -p "$RESULTS_DIR"

LOG="$RESULTS_DIR/synthetic-checkpoint-${AGENT}.txt"
: > "$LOG"

pass() { echo "PASS: $1" | tee -a "$LOG"; }
warn() { echo "WARN: $1" | tee -a "$LOG"; }
fail() { echo "FAIL: $1" | tee -a "$LOG"; exit 1; }

echo "=== Synthetic checkpoint test for: $AGENT ===" | tee "$LOG"

cd "$REPO_DIR"

# Create a test file representing agent output
TEST_FILE="agent-test-${AGENT}.txt"
echo "Hello from $AGENT synthetic test" > "$TEST_FILE"
git add "$TEST_FILE"
pass "Test file created and staged: $TEST_FILE"

# Feed synthetic checkpoint data using the agent-v1 preset.
# This format is documented and stable; the agent-v1 preset handles it uniformly.
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)
CHECKPOINT_JSON=$(printf '{
  "type": "ai_agent",
  "repo_working_dir": "%s",
  "edited_filepaths": ["%s"],
  "transcript": {
    "messages": [
      {
        "role": "user",
        "content": "Create %s for synthetic CI test"
      },
      {
        "role": "assistant",
        "content": "Creating the file now.",
        "tool_calls": []
      }
    ]
  },
  "agent_name": "%s",
  "model": "synthetic-test-model",
  "conversation_id": "synthetic-%s-%s"
}' "$REPO_DIR" "$TEST_FILE" "$TEST_FILE" "$AGENT" "$AGENT" "$(date +%s)")

echo "$CHECKPOINT_JSON" | git-ai checkpoint agent-v1 --hook-input stdin \
  || fail "git-ai checkpoint agent-v1 command failed"
pass "Synthetic checkpoint accepted by git-ai"

# Commit the staged file
git commit -m "Synthetic $AGENT checkpoint test" \
  || fail "git commit failed after synthetic checkpoint"
pass "Commit created successfully"

# Verify authorship note was generated
if git notes --ref=ai show HEAD 2>/dev/null \
    | grep -qiE "authorship|schema_version|prompts"; then
  pass "Authorship note found on HEAD"
else
  fail "No authorship note found on HEAD (post-commit hook may not have fired)"
fi

# Verify the blame output mentions AI attribution (non-fatal)
if git-ai blame "$TEST_FILE" 2>/dev/null | grep -qiE "$AGENT|ai|attribution"; then
  pass "AI attribution visible in blame output"
else
  warn "AI attribution not found in blame output for $AGENT (non-fatal)"
fi

echo "=== Synthetic checkpoint test COMPLETE for: $AGENT ===" | tee -a "$LOG"
