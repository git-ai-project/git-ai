#!/usr/bin/env bash
# Verify that the agent left proper git-ai attribution after its live run.
# Usage: verify-attribution.sh <agent>
# Expects: /tmp/test-repo to exist with the agent's commit
set -euo pipefail

AGENT="${1:?Usage: $0 <agent>}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/test-results}"
mkdir -p "$RESULTS_DIR"

LOG="$RESULTS_DIR/attribution-${AGENT}.txt"
: > "$LOG"

pass() { echo "PASS: $1" | tee -a "$LOG"; }
warn() { echo "WARN: $1" | tee -a "$LOG"; }
fail() { echo "FAIL: $1" | tee -a "$LOG"; exit 1; }

echo "=== Verifying attribution for: $AGENT ===" | tee "$LOG"

cd /tmp/test-repo

# ── File existence ──────────────────────────────────────────────────────────
[ -f hello.txt ] || fail "hello.txt was not created by agent"
pass "hello.txt exists"

grep -q "Hello World" hello.txt \
  || fail "hello.txt does not contain 'Hello World'"
pass "hello.txt contains expected content"

# ── Commit existence ────────────────────────────────────────────────────────
COMMITS=$(git log --oneline | wc -l | tr -d ' ')
[ "$COMMITS" -ge 2 ] \
  || fail "Agent did not create a commit (only $COMMITS commit in log)"
pass "Agent commit found ($COMMITS total commits)"

# ── Authorship note ─────────────────────────────────────────────────────────
if git notes --ref=ai show HEAD 2>/dev/null \
    | grep -qiE "authorship|schema_version|prompts"; then
  pass "Authorship note found on HEAD"

  # Try to verify agent identification (best-effort; note structure may vary)
  if git notes --ref=ai show HEAD 2>/dev/null \
      | python3 -c "
import json, sys
try:
    note = json.load(sys.stdin)
    prompts = note.get('prompts', [])
    found = any('$AGENT' in str(p.get('agent_id', {}).get('tool', '')).lower()
                for p in prompts)
    sys.exit(0 if found else 1)
except Exception:
    sys.exit(1)
" 2>/dev/null; then
    pass "Agent '$AGENT' identified in authorship note"
  else
    warn "Agent '$AGENT' not found in authorship note prompts (hook integration may be partial)"
  fi
else
  warn "No authorship note on HEAD (git-ai hook may not have fired for this agent version)"
fi

# ── Blame output ────────────────────────────────────────────────────────────
if git-ai blame hello.txt 2>/dev/null | grep -qiE "$AGENT|ai-generated|attribution"; then
  pass "AI attribution visible in 'git-ai blame' output"
else
  warn "'git-ai blame' did not show AI attribution (non-fatal; checkpoint hook integration may be pending)"
fi

echo "=== Attribution verification COMPLETE for: $AGENT ===" | tee -a "$LOG"
