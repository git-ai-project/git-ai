#!/usr/bin/env bash
# Run the real agent CLI with a deterministic prompt in a test repository.
# Usage: test-live-agent.sh <agent>
# Expects: /tmp/test-repo to exist with git-ai hooks installed
# Expects: relevant API key env var to be set by caller
set -euo pipefail

AGENT="${1:?Usage: $0 <agent>}"

PROMPT="Create a file called hello.txt containing exactly the text 'Hello World' on a single line. Then stage and commit it with the message 'Add hello.txt'."

cd /tmp/test-repo

echo "=== Running live agent: $AGENT ==="
echo "Prompt: $PROMPT"

case "$AGENT" in
  claude)
    timeout 300 claude -p \
      --dangerously-skip-permissions \
      --max-turns 3 \
      "$PROMPT"
    ;;

  codex)
    timeout 300 codex exec --full-auto "$PROMPT"
    ;;

  gemini)
    # Pre-install ripgrep to avoid Gemini CLI initialization hang on headless Linux
    which rg 2>/dev/null || (apt-get install -y ripgrep 2>/dev/null || true)
    timeout 300 gemini --approval-mode=yolo "$PROMPT"
    ;;

  droid)
    timeout 300 droid exec --auto high "$PROMPT"
    ;;

  opencode)
    # OpenCode can hang in containers; use extra timeout handling
    timeout 240 opencode run --command "$PROMPT" || {
      echo "WARN: opencode timed out or failed — checking if file was created"
      [ -f hello.txt ] && echo "File exists despite error; continuing"
    }
    ;;

  *)
    echo "ERROR: Unknown agent: $AGENT"
    exit 1
    ;;
esac

echo "=== Live agent run COMPLETE for: $AGENT ==="
