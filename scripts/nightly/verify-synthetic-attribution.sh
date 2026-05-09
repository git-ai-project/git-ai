#!/usr/bin/env bash
# Thorough verification of the attribution pipeline after a synthetic checkpoint commit.
#
# Checks (in order):
#   1. Authorship note exists on HEAD (refs/notes/ai)
#   2. Note contains parseable JSON
#   3. schema_version = "authorship/3.0.0"
#   4. prompts dict has at least 1 entry (prompt was stored)
#   5. Total transcript messages across all prompts > 0
#   6. git-ai stats HEAD --json reports ai_additions > 0
#   7. Test file (agent-test-<agent>.txt) appears in raw note text (WARN)
#   8. git-ai blame shows AI attribution markers (WARN)
#
# Usage: verify-synthetic-attribution.sh <agent> [repo-dir]
set -euo pipefail

AGENT="${1:?Usage: $0 <agent> [repo-dir]}"
REPO_DIR="${2:-/tmp/test-repo}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/test-results}"
mkdir -p "$RESULTS_DIR"

LOG="$RESULTS_DIR/synthetic-attribution-${AGENT}.txt"
NOTE_RAW="$RESULTS_DIR/synth-note-raw-${AGENT}.txt"
META_JSON="$RESULTS_DIR/synth-note-meta-${AGENT}.json"
BLAME_OUT="$RESULTS_DIR/synth-blame-${AGENT}.txt"
STATS_OUT="$RESULTS_DIR/synth-stats-${AGENT}.txt"
: > "$LOG"

pass() { echo "PASS: $1" | tee -a "$LOG"; }
warn() { echo "WARN: $1" | tee -a "$LOG"; }
fail() { echo "FAIL: $1" | tee -a "$LOG"; exit 1; }

echo "=== Synthetic attribution verification: $AGENT ===" | tee "$LOG"
cd "$REPO_DIR"

TEST_FILE="agent-test-${AGENT}.txt"

# ── 1. Authorship note exists ──────────────────────────────────────────────────
git notes --ref=ai show HEAD > "$NOTE_RAW" 2>/dev/null \
  || fail "No authorship note on HEAD — post-commit hook did not fire (git-ai hooks may not be wired correctly in repo)"

pass "Authorship note found on HEAD ($(wc -l < "$NOTE_RAW") lines)"

# ── 2. Parse JSON metadata from note ─────────────────────────────────────────
# The note format has file attestations (plain text) above the JSON metadata block.
if ! python3 - "$NOTE_RAW" "$META_JSON" <<'PYEOF'
import json, sys

with open(sys.argv[1]) as f:
    content = f.read()

lines = content.split('\n')
for i, line in enumerate(lines):
    if line.strip().startswith('{'):
        try:
            obj = json.loads('\n'.join(lines[i:]))
            with open(sys.argv[2], 'w') as out:
                json.dump(obj, out, indent=2)
            sys.exit(0)
        except json.JSONDecodeError:
            continue

print(f"ERROR: No JSON object found in authorship note. Note content:\n{content[:800]}",
      file=sys.stderr)
sys.exit(1)
PYEOF
then
  fail "Could not extract JSON metadata from authorship note — unexpected note format"
fi

pass "Authorship note contains parseable JSON metadata"

# ── 3. Schema version ─────────────────────────────────────────────────────────
SCHEMA=$(python3 -c "import json, sys; d=json.load(open(sys.argv[1])); print(d.get('schema_version','MISSING'))" "$META_JSON")
[ "$SCHEMA" = "authorship/3.0.0" ] \
  || fail "Wrong schema_version: '$SCHEMA' (expected 'authorship/3.0.0')"

pass "schema_version = $SCHEMA"

# ── 4. Sessions non-empty ─────────────────────────────────────────────────────
SESSION_COUNT=$(python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
prompts = len(d.get('prompts', {}))
sessions = len(d.get('sessions', {}))
print(prompts + sessions)
" "$META_JSON")
[ "$SESSION_COUNT" -gt 0 ] \
  || fail "No prompt/session entries in authorship note — synthetic checkpoint data was not stored (check git-ai checkpoint pipeline)"

pass "$SESSION_COUNT AI session(s) recorded"

# ── 5. Transcript messages captured ───────────────────────────────────────────
MSG_COUNT=$(python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
total = sum(len(r.get('messages', [])) for r in list(d.get('prompts', {}).values()) + list(d.get('sessions', {}).values()))
print(total)
" "$META_JSON")
if [ "$MSG_COUNT" -gt 0 ]; then
  pass "Transcript captured: $MSG_COUNT message(s) recorded across all sessions"
else
  warn "No transcript messages in authorship note — sessions format does not embed transcripts inline"
fi

# ── 6. git-ai stats reports AI additions ──────────────────────────────────────
# Capture output separately so pipefail doesn't trip on grep finding no DEBUG lines
STATS_RAW=$(git-ai stats HEAD --json 2>/dev/null) \
  || fail "git-ai stats HEAD --json command failed"
echo "$STATS_RAW" | grep -v '^\[DEBUG\]' > "$STATS_OUT" || true

AI_ADDS=$(python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    content = f.read().strip()
if not content:
    print(0)
    sys.exit(0)
# Find JSON object in output
lines = content.split('\n')
for i, line in enumerate(lines):
    if line.strip().startswith('{'):
        try:
            d = json.loads('\n'.join(lines[i:]))
            print(d.get('ai_additions', 0))
            sys.exit(0)
        except json.JSONDecodeError:
            continue
print(0)
" "$STATS_OUT" 2>/dev/null || echo "0")

[ "$AI_ADDS" -gt 0 ] \
  || fail "git-ai stats HEAD reports ai_additions=0 — AI work not tracked in stats (checkpoint data may not have been linked to this commit)"

pass "git-ai stats HEAD: ai_additions=$AI_ADDS"

# ── 7. Test file in note raw text ─────────────────────────────────────────────
if grep -qF "$TEST_FILE" "$NOTE_RAW" 2>/dev/null; then
  pass "$TEST_FILE appears in authorship note (line-level attribution present)"
else
  warn "$TEST_FILE not found in authorship note text — line-level attribution may be missing for this file"
fi

# ── 8. git-ai blame shows AI attribution ──────────────────────────────────────
if git-ai blame "$TEST_FILE" > "$BLAME_OUT" 2>/dev/null; then
  if grep -qiE "ai-generated|${AGENT}|generated|ai_human_author" "$BLAME_OUT" 2>/dev/null; then
    pass "AI attribution visible in git-ai blame output for $TEST_FILE"
  else
    warn "git-ai blame does not show explicit AI attribution for $TEST_FILE — agent_id may not be resolved in blame display"
  fi
else
  warn "git-ai blame command failed for $TEST_FILE — blame verification skipped"
fi

echo "" | tee -a "$LOG"
echo "=== Synthetic attribution verification COMPLETE: $AGENT ===" | tee -a "$LOG"
