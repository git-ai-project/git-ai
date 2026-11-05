#!/usr/bin/env bash
# git-ai attribution hook for Crush.
#
# Crush only supports PreToolUse hooks (no PostToolUse), so this wrapper
# synthesizes the post-edit checkpoint:
#
#   1. Flush: when a new PreToolUse fires, the previous tool call has
#      necessarily finished (Crush executes tools sequentially within a
#      session), so any pending post checkpoint is emitted immediately.
#      This captures edits from arbitrarily long-running tool calls.
#   2. Pre-edit: the native payload is forwarded to `git-ai checkpoint
#      crush` as a pre-edit (human) checkpoint.
#   3. Fallback timer: if no further tool call arrives (last tool of a
#      turn), a delayed background job emits the pending post checkpoint.
#      The timer is superseded and skipped whenever a newer invocation
#      flushes first, so it cannot capture a later tool call's work.
#
# Requires: jq
set -uo pipefail

GIT_AI_BIN='__GIT_AI_BINARY_PATH__'
# Delay for the last-tool-of-a-turn fallback, in seconds.
FALLBACK_DELAY=${GIT_AI_CRUSH_POST_DELAY:-10}

input=$(cat)

project_dir=${CRUSH_PROJECT_DIR:-${CRUSH_CWD:-$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)}}
session_id=${CRUSH_SESSION_ID:-$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)}
session_id=${session_id:-unknown}

# Every bash invocation is checkpointed, including commands that only look
# read-only: a textual filter cannot prove a command cannot write tracked
# files (e.g. `python3 -c 'Path(...).write_text(...)'`, `perl -e`,
# `git checkout`), and a missed write would silently lose AI attribution.
# The extra checkpoints on genuinely read-only commands are cheap and
# attribute nothing.

state_dir="${TMPDIR:-/tmp}/git-ai-crush/$session_id"
mkdir -p "$state_dir" 2>/dev/null || exit 0
counter_file="$state_dir/counter"
[[ -f "$counter_file" ]] || echo 0 >"$counter_file"
invocation=$(( $(cat "$counter_file" 2>/dev/null || echo 0) + 1 ))
echo "$invocation" >"$counter_file"

emit_pending_post() {
  local pending="$state_dir/pending"
  if [[ -f "$pending" && ! -f "$pending.done" ]]; then
    touch "$pending.done"
    jq -c '. + {hook_event_name: "PostToolUse"}' "$pending" \
      | "$GIT_AI_BIN" checkpoint crush --hook-input stdin >/dev/null 2>&1
    rm -f "$pending" "$pending.done"
  fi
}

# 1) The previous tool call has completed (sequential execution); attribute
#    its edits now, before this invocation's human pre-mark runs.
emit_pending_post

# 2) Pre-edit checkpoint: mark the currently dirty state as human-authored.
#    This baseline separates earlier (possibly human) edits from the tool's
#    changes. If it fails (e.g. the daemon is unavailable), the synthetic
#    post would credit those earlier edits to the AI, so abort instead.
#    Exiting 0 is non-blocking for Crush; attribution simply falls back to
#    human for this invocation.
if ! printf '%s' "$input" | "$GIT_AI_BIN" checkpoint crush --hook-input stdin >/dev/null 2>&1; then
  exit 0
fi

# 3) Persist this invocation's payload for a later flush.
printf '%s' "$input" >"$state_dir/pending"

(
  sleep "$FALLBACK_DELAY"
  # Skip if a newer invocation superseded us: its synchronous flush owns
  # attribution from here on. This prevents an earlier timer from
  # capturing a later tool call's work.
  if [[ "$(cat "$counter_file" 2>/dev/null)" == "$invocation" ]]; then
    emit_pending_post
  fi
) >/dev/null 2>&1 &

exit 0
