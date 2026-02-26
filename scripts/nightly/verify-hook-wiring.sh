#!/usr/bin/env bash
# Verify that git-ai installed hooks correctly for the given agent.
# Usage: verify-hook-wiring.sh <agent>
# Agent must be one of: claude, codex, gemini, droid, opencode
set -euo pipefail

AGENT="${1:?Usage: $0 <agent>}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/test-results}"
mkdir -p "$RESULTS_DIR"

LOG="$RESULTS_DIR/hook-wiring-${AGENT}.txt"
: > "$LOG"

pass() { echo "PASS: $1" | tee -a "$LOG"; }
fail() { echo "FAIL: $1" | tee -a "$LOG"; exit 1; }

echo "=== Verifying hook wiring for: $AGENT ===" | tee "$LOG"

case "$AGENT" in
  claude)
    SETTINGS="$HOME/.claude/settings.json"
    [ -f "$SETTINGS" ] || fail "settings.json not found at $SETTINGS"
    grep -q "checkpoint claude" "$SETTINGS" \
      || fail "checkpoint claude hook not found in $SETTINGS"
    pass "Claude Code hooks configured in $SETTINGS"
    ;;

  codex)
    CONFIG="$HOME/.codex/config.toml"
    [ -f "$CONFIG" ] || fail "config.toml not found at $CONFIG"
    grep -q "checkpoint codex" "$CONFIG" \
      || fail "checkpoint codex hook not found in $CONFIG"
    pass "Codex hooks configured in $CONFIG"
    ;;

  gemini)
    SETTINGS="$HOME/.gemini/settings.json"
    [ -f "$SETTINGS" ] || fail "settings.json not found at $SETTINGS"
    grep -q "checkpoint gemini" "$SETTINGS" \
      || fail "checkpoint gemini hook not found in $SETTINGS"
    pass "Gemini CLI hooks configured in $SETTINGS"
    ;;

  droid)
    SETTINGS="$HOME/.factory/settings.json"
    [ -f "$SETTINGS" ] || fail "settings.json not found at $SETTINGS"
    grep -q "checkpoint droid" "$SETTINGS" \
      || fail "checkpoint droid hook not found in $SETTINGS"
    pass "Droid hooks configured in $SETTINGS"
    ;;

  opencode)
    PLUGIN="$HOME/.config/opencode/plugin/git-ai.ts"
    [ -f "$PLUGIN" ] || fail "OpenCode plugin not found at $PLUGIN"
    pass "OpenCode git-ai plugin installed at $PLUGIN"
    ;;

  *)
    fail "Unknown agent: $AGENT (must be: claude, codex, gemini, droid, opencode)"
    ;;
esac

echo "=== Hook wiring verification PASSED for: $AGENT ===" | tee -a "$LOG"
