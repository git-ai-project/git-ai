#!/usr/bin/env bash
# git-ai attribution hook for Crush.
#
# Crush only supports PreToolUse hooks (no PostToolUse), so this wrapper:
#   1. forwards the native hook payload to `git-ai checkpoint crush` as a
#      pre-edit (human) checkpoint, and
#   2. re-invokes the same payload with hook_event_name=PostToolUse in the
#      background after a short delay, once the tool call has completed, so
#      the edit is attributed to the AI agent.
#
# Requires: jq
set -uo pipefail

GIT_AI_BIN='__GIT_AI_BINARY_PATH__'

input=$(cat)

tool_name=${CRUSH_TOOL_NAME:-$(printf '%s' "$input" | jq -r '.tool_name // empty' 2>/dev/null)}
project_dir=${CRUSH_PROJECT_DIR:-${CRUSH_CWD:-$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)}}
session_id=${CRUSH_SESSION_ID:-$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)}

# Read-only bash commands cannot modify tracked files; skip them to avoid
# checkpoint churn on every shell call.
if [[ "$tool_name" == "bash" ]]; then
  cmd=${CRUSH_TOOL_INPUT_COMMAND:-$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)}
  printf '%s' "$cmd" | grep -qE '(>>|>[[:space:]]*[^&>]|sed -i|tee |cp |mv |rm |mkdir |touch |truncate |dd |patch |git apply)' || exit 0
fi

# Pre-edit checkpoint: mark the currently dirty state as human-authored.
printf '%s' "$input" | "$GIT_AI_BIN" checkpoint crush --hook-input stdin >/dev/null 2>&1

# Post-edit checkpoint: attribute the completed tool call to the AI agent.
# Crush runs hooks before the tool executes, so we delay in the background.
(
  sleep 5
  printf '%s' "$input" \
    | jq -c --arg sid "$session_id" --arg cwd "$project_dir" \
        '. + {hook_event_name: "PostToolUse", session_id: (if $sid != "" then $sid else .session_id end), cwd: (if $cwd != "" then $cwd else .cwd end)}' \
    | "$GIT_AI_BIN" checkpoint crush --hook-input stdin >/dev/null 2>&1
) >/dev/null 2>&1 &

exit 0
