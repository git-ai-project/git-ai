#!/usr/bin/env bash
# Full end-to-end live agent integration test.
# Seeds the test repo with a real Python module, runs the real agent CLI with a
# substantive coding prompt, and ensures changes are committed so the
# post-commit hook can write an authorship note.
#
# Usage: test-live-agent.sh <agent>
# Expects: TEST_REPO_DIR (default /tmp/test-repo) pre-created with git-ai hooks installed
# Expects: relevant API key env var to be set by caller
set -euo pipefail

AGENT="${1:?Usage: $0 <agent>}"
REPO_DIR="${TEST_REPO_DIR:-/tmp/test-repo}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/test-results}"
mkdir -p "$RESULTS_DIR"

LOG="$RESULTS_DIR/live-agent-${AGENT}.txt"
: > "$LOG"

pass() { echo "PASS: $1" | tee -a "$LOG"; }
warn() { echo "WARN: $1" | tee -a "$LOG"; }
fail() { echo "FAIL: $1" | tee -a "$LOG"; exit 1; }

echo "=== Live agent integration test: $AGENT ===" | tee "$LOG"
cd "$REPO_DIR"

# ── Seed the repo with a real Python module (idempotent across retries) ──────
if [ ! -f utils/math_utils.py ]; then
  mkdir -p utils
  cat > utils/math_utils.py <<'PYEOF'
"""Utility functions for mathematical operations."""


def add(a: int, b: int) -> int:
    """Return the sum of two integers."""
    return a + b


def subtract(a: int, b: int) -> int:
    """Return the difference of two integers."""
    return a - b


def is_prime(n: int) -> bool:
    """Return True if n is a prime number."""
    if n < 2:
        return False
    for i in range(2, int(n**0.5) + 1):
        if n % i == 0:
            return False
    return True
PYEOF
  git add utils/math_utils.py
  git commit -m "Add initial math utilities module"
  pass "Seeded repository with utils/math_utils.py"
else
  pass "Repository already seeded with utils/math_utils.py (retry attempt)"
fi

# ── Run the real agent CLI ────────────────────────────────────────────────────
COMMITS_BEFORE=$(git rev-list HEAD --count)

PROMPT="Add a function called fibonacci(n) to utils/math_utils.py that returns the nth Fibonacci number (0-indexed: fibonacci(0)=0, fibonacci(1)=1) using an iterative approach. Stage the file and commit it with the message 'Add fibonacci function'."

echo "Agent:  $AGENT" | tee -a "$LOG"
echo "Prompt: $PROMPT" | tee -a "$LOG"
echo "" | tee -a "$LOG"

case "$AGENT" in
  claude)
    timeout 300 claude -p \
      --dangerously-skip-permissions \
      --max-turns 5 \
      "$PROMPT" 2>&1 | tee -a "$LOG" || warn "claude exited with non-zero status"
    ;;

  codex)
    timeout 300 codex exec --full-auto "$PROMPT" 2>&1 | tee -a "$LOG" \
      || warn "codex exited with non-zero status"
    ;;

  gemini)
    # Pre-install ripgrep to avoid Gemini CLI initialization hang on headless Linux
    which rg 2>/dev/null || apt-get install -y ripgrep 2>/dev/null || true
    timeout 300 gemini --approval-mode=yolo "$PROMPT" 2>&1 | tee -a "$LOG" \
      || warn "gemini exited with non-zero status"
    ;;

  droid)
    timeout 300 droid exec --auto high "$PROMPT" 2>&1 | tee -a "$LOG" \
      || warn "droid exited with non-zero status"
    ;;

  opencode)
    timeout 240 opencode run --command "$PROMPT" 2>&1 | tee -a "$LOG" \
      || warn "opencode exited with non-zero status"
    ;;

  *)
    fail "Unknown agent: $AGENT"
    ;;
esac

# ── Ensure changes are committed ──────────────────────────────────────────────
# The post-commit hook writes the authorship note, so we need a commit.
# If the agent wrote code but didn't commit, do a fallback commit — the
# pre/post-tool-use hooks still fired during the agent run, so working log
# data is present and the post-commit hook will still produce a note.
COMMITS_AFTER=$(git rev-list HEAD --count)

if [ "$COMMITS_AFTER" -gt "$COMMITS_BEFORE" ]; then
  pass "Agent committed its work ($(( COMMITS_AFTER - COMMITS_BEFORE )) new commit(s))"
else
  if [ -n "$(git status --porcelain)" ]; then
    warn "Agent did not commit — staging all changes and committing as fallback"
    git add -A
    git commit -m "Add fibonacci function (fallback commit for CI)" \
      || fail "Fallback commit failed — check agent output above"
    pass "Fallback commit created"
  else
    fail "Agent made no changes to the repository — expected fibonacci function in utils/math_utils.py"
  fi
fi

echo "" | tee -a "$LOG"
echo "=== Live agent test COMPLETE: $AGENT ===" | tee -a "$LOG"
