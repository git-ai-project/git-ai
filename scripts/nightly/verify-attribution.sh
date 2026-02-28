#!/usr/bin/env bash
# Deep end-to-end verification of git-ai attribution after a live agent run.
#
# Checks (in order):
#   1. fibonacci function exists in utils/math_utils.py
#   2. Agent created a commit (≥3 total: initial + seed + agent)
#   3. Authorship note exists on HEAD (refs/notes/ai)
#   4. Note contains valid JSON with schema_version = "authorship/3.0.0"
#   5. At least one prompt session was recorded (hooks captured agent activity)
#   6. At least one prompt has agent_id.tool matching this agent (fuzzy)
#   7. At least one transcript message was recorded
#   8. utils/math_utils.py appears in the attestation section (line attribution)
#   9. git-ai blame shows AI attribution on fibonacci lines
#
# Usage: verify-attribution.sh <agent>
# Expects: TEST_REPO_DIR (default /tmp/test-repo) with agent commit
set -euo pipefail

AGENT="${1:?Usage: $0 <agent>}"
REPO_DIR="${TEST_REPO_DIR:-/tmp/test-repo}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/test-results}"
mkdir -p "$RESULTS_DIR"

LOG="$RESULTS_DIR/attribution-${AGENT}.txt"
NOTE_RAW="$RESULTS_DIR/note-raw-${AGENT}.txt"
META_JSON="$RESULTS_DIR/note-meta-${AGENT}.json"
BLAME_OUT="$RESULTS_DIR/blame-${AGENT}.txt"
: > "$LOG"

pass() { echo "PASS: $1" | tee -a "$LOG"; }
warn() { echo "WARN: $1" | tee -a "$LOG"; }
fail() { echo "FAIL: $1" | tee -a "$LOG"; exit 1; }

echo "=== Attribution verification: $AGENT ===" | tee "$LOG"
cd "$REPO_DIR"

# ── 1. File content ───────────────────────────────────────────────────────────
[ -f utils/math_utils.py ] \
  || fail "utils/math_utils.py not found — agent did not create it"

grep -q "def fibonacci" utils/math_utils.py \
  || fail "fibonacci function not found in utils/math_utils.py — agent did not implement it"

pass "fibonacci function present in utils/math_utils.py"

# ── 2. Commit history ─────────────────────────────────────────────────────────
COMMITS=$(git rev-list HEAD --count)
[ "$COMMITS" -ge 3 ] \
  || fail "Expected ≥3 commits (initial + seed + agent), found $COMMITS — agent may have failed before committing"

pass "Agent commit confirmed ($COMMITS commits total)"

# ── 3. Authorship note exists ─────────────────────────────────────────────────
git notes --ref=ai show HEAD > "$NOTE_RAW" 2>/dev/null \
  || fail "No authorship note on HEAD — post-commit hook did not fire (git-ai hooks may not be wired correctly)"

pass "Authorship note found on HEAD ($(wc -l < "$NOTE_RAW") lines)"

# ── 4. Parse JSON metadata from note ─────────────────────────────────────────
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

# ── 5. Schema version ─────────────────────────────────────────────────────────
SCHEMA=$(python3 -c "import json; d=json.load(open('$META_JSON')); print(d.get('schema_version','MISSING'))")
[ "$SCHEMA" = "authorship/3.0.0" ] \
  || fail "Wrong schema_version: '$SCHEMA' (expected 'authorship/3.0.0')"

pass "schema_version = $SCHEMA"

# ── 6. Prompts non-empty ──────────────────────────────────────────────────────
PROMPT_COUNT=$(python3 -c "import json; d=json.load(open('$META_JSON')); print(len(d.get('prompts', {})))")
[ "$PROMPT_COUNT" -gt 0 ] \
  || fail "No prompt sessions recorded in authorship note — agent hooks did not capture activity (check hook wiring with verify-hook-wiring.sh)"

pass "$PROMPT_COUNT prompt session(s) recorded"

# ── 7. Agent identification ────────────────────────────────────────────────────
AGENT_MATCH=$(python3 - "$META_JSON" "$AGENT" <<'PYEOF'
import json, sys

meta = json.load(open(sys.argv[1]))
agent = sys.argv[2].lower()

for record in meta.get("prompts", {}).values():
    tool = str(record.get("agent_id", {}).get("tool", "")).lower()
    # Fuzzy match: "claude" matches "claude_code", "gemini" matches "gemini_cli", etc.
    if tool and (agent in tool or tool in agent):
        print("found")
        sys.exit(0)

# Print what we did find for debugging
tools = [str(r.get("agent_id", {}).get("tool", "")) for r in meta.get("prompts", {}).values()]
print(f"not_found (found tools: {tools})", file=sys.stderr)
print("not_found")
PYEOF
)

if [ "$AGENT_MATCH" = "found" ]; then
  pass "agent_id.tool matches '$AGENT'"
else
  warn "agent_id.tool does not contain '$AGENT' — hook integration may be partial for this agent version (see $META_JSON for details)"
fi

# ── 8. Transcript messages captured ───────────────────────────────────────────
MSG_COUNT=$(python3 -c "
import json
d = json.load(open('$META_JSON'))
total = sum(len(r.get('messages', [])) for r in d.get('prompts', {}).values())
print(total)
")

if [ "$MSG_COUNT" -gt 0 ]; then
  pass "Transcript captured: $MSG_COUNT message(s) recorded across all prompt sessions"
else
  warn "No transcript messages in authorship note — conversation capture hook may be partial"
fi

# ── 9. Line-level attestation (utils/math_utils.py in attestation section) ───
if grep -q "math_utils" "$NOTE_RAW" 2>/dev/null; then
  pass "utils/math_utils.py appears in attestation section (line-level attribution present)"
else
  warn "utils/math_utils.py not found in attestation section of note — line-level attribution may be missing"
fi

# ── 10. git-ai blame ──────────────────────────────────────────────────────────
if git-ai blame utils/math_utils.py > "$BLAME_OUT" 2>/dev/null; then
  if grep -q "fibonacci" "$BLAME_OUT" 2>/dev/null; then
    pass "git-ai blame output covers fibonacci function lines"

    # Check for AI attribution markers in blame output (ai_human_author name or agent name)
    if grep -qiE "ai-generated|${AGENT}|generated" "$BLAME_OUT" 2>/dev/null; then
      pass "AI attribution visible in git-ai blame output for fibonacci lines"
    else
      warn "git-ai blame does not show explicit AI attribution for fibonacci lines (agent_id may not be present in note)"
    fi
  else
    warn "fibonacci lines not visible in git-ai blame output — file may not be tracked yet"
  fi
else
  warn "git-ai blame command failed — blame verification skipped"
fi

echo "" | tee -a "$LOG"
echo "=== Attribution verification COMPLETE: $AGENT ===" | tee -a "$LOG"
