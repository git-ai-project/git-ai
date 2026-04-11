#!/usr/bin/env bash
# git-ai-zed-hook.sh
#
# Zed invokes this script as a format_on_save external formatter:
#   - File content is passed via stdin.
#   - The formatted result must be written to stdout.
#   - Non-zero exit causes Zed to show an error and discard stdout.
#
# We pass the content through unchanged (no formatting) while firing the
# git-ai known_human checkpoint in the background.
#
# Zed sets $ZED_FILE to the absolute path of the file being saved and
# $ZED_ROW / $ZED_COLUMN to the cursor position.  The working directory is
# the worktree root.
#
# Debounce: 500ms, implemented via a per-repo lock file and a background
# sleep.  Multiple rapid saves within 500ms collapse into a single checkpoint
# call because only the last writer wins the lock.

set -euo pipefail

# ------------------------------------------------------------------
# 1. Capture stdin so we can both pass it through AND read it below.
# ------------------------------------------------------------------
CONTENT="$(cat)"

# ------------------------------------------------------------------
# 2. Emit content unchanged (the formatter contract).
# ------------------------------------------------------------------
printf '%s' "$CONTENT"

# ------------------------------------------------------------------
# 3. Resolve inputs.
# ------------------------------------------------------------------
FILE_PATH="${ZED_FILE:-}"
if [[ -z "$FILE_PATH" ]]; then
    # ZED_FILE not set — skip checkpoint (can happen in tests)
    exit 0
fi

# Find repo root by running git in the file's directory
FILE_DIR="$(dirname "$FILE_PATH")"
REPO_ROOT="$(git -C "$FILE_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$REPO_ROOT" ]]; then
    exit 0
fi

# Locate the git-ai binary
GIT_AI_BIN="__GIT_AI_BINARY_PATH__"
if [[ ! -x "$GIT_AI_BIN" ]]; then
    # Fall back to PATH
    GIT_AI_BIN="$(command -v git-ai 2>/dev/null || true)"
fi
if [[ -z "$GIT_AI_BIN" || ! -x "$GIT_AI_BIN" ]]; then
    exit 0
fi

# ------------------------------------------------------------------
# 4. Debounce: 500ms per repo root using a lock file.
# ------------------------------------------------------------------
# We write our PID + timestamp into a per-repo lock file.  After 500ms we
# check whether we are still the most-recent writer; if so, fire the
# checkpoint.  This is a best-effort debounce — concurrent saves within the
# window collapse naturally.

LOCK_DIR="${TMPDIR:-/tmp}/git-ai-zed-debounce"
mkdir -p "$LOCK_DIR"

# Hash the repo root path to create a safe filename
REPO_HASH="$(printf '%s' "$REPO_ROOT" | sha256sum | cut -c1-16 2>/dev/null || printf '%s' "$REPO_ROOT" | md5sum | cut -c1-16)"
LOCK_FILE="$LOCK_DIR/$REPO_HASH"
MY_TOKEN="$$-$(date +%s%N 2>/dev/null || date +%s)"

# Build the JSON payload now (synchronously, before backgrounding)
# so we capture the exact content at save time.
ESCAPED_FILE_PATH="$(printf '%s' "$FILE_PATH" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' 2>/dev/null || { _v="$(printf '%s' "$FILE_PATH" | sed 's/\\/\\\\/g; s/"/\\"/g')"; printf '"%s"' "$_v"; })"
ESCAPED_CONTENT="$(printf '%s' "$CONTENT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' 2>/dev/null || printf '""')"
ESCAPED_REPO="$(printf '%s' "$REPO_ROOT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' 2>/dev/null || { _v="$(printf '%s' "$REPO_ROOT" | sed 's/\\/\\\\/g; s/"/\\"/g')"; printf '"%s"' "$_v"; })"

HOOK_INPUT="{\"editor\":\"zed\",\"editor_version\":\"unknown\",\"extension_version\":\"1.0.0\",\"cwd\":$ESCAPED_REPO,\"edited_filepaths\":[$ESCAPED_FILE_PATH],\"dirty_files\":{$ESCAPED_FILE_PATH:$ESCAPED_CONTENT}}"

# Fire in background with debounce
{
    printf '%s' "$MY_TOKEN" > "$LOCK_FILE"
    sleep 0.5
    CURRENT_TOKEN="$(cat "$LOCK_FILE" 2>/dev/null || true)"
    if [[ "$CURRENT_TOKEN" == "$MY_TOKEN" ]]; then
        printf '%s' "$HOOK_INPUT" | "$GIT_AI_BIN" checkpoint known_human --hook-input stdin \
            >/dev/null 2>&1 || true
    fi
} &
disown $! 2>/dev/null || true

exit 0
