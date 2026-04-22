#!/usr/bin/env bash
# Start the git-ai async daemon for CI workflows.
#
# Usage:  source scripts/nightly/start-async-daemon.sh <git-ai-binary> [real-git-path]
#
# The script:
#   1. Creates ~/.git-ai/config.json for the daemon
#   2. Picks socket paths under RUNNER_TEMP (or /tmp)
#   3. Starts the daemon in the background
#   4. Waits for sockets to appear (up to 10 s)
#   5. Exports env vars to GITHUB_ENV so subsequent steps inherit them
#
# After sourcing, the following env vars are set in the current shell AND
# appended to GITHUB_ENV (if it exists):
#   GIT_AI_TEST_FORCE_TTY, GIT_AI_POST_COMMIT_TIMEOUT_MS,
#   GIT_AI_DAEMON_HOME, GIT_AI_DAEMON_CONTROL_SOCKET, GIT_AI_DAEMON_TRACE_SOCKET,
#   ASYNC_DAEMON_PID
set -euo pipefail

GIT_AI_BIN="${1:?Usage: source start-async-daemon.sh <path-to-git-ai-binary> [real-git-path]}"

# ── Locate real git (not the git-ai proxy) ───────────────────────────────────
# The caller can pass an explicit path; otherwise probe common locations so we
# never accidentally point the daemon config at the git-ai proxy symlink.
REAL_GIT="${2:-}"
if [ -z "$REAL_GIT" ]; then
    for candidate in /usr/bin/git /usr/local/bin/git; do
        if [ -x "$candidate" ]; then
            REAL_GIT="$candidate"
            break
        fi
    done
    # Last resort: use whatever is on PATH.
    if [ -z "$REAL_GIT" ]; then
        REAL_GIT="$(command -v git)"
    fi
fi

# ── Daemon home directory ────────────────────────────────────────────────────
DAEMON_HOME=$(mktemp -d "${RUNNER_TEMP:-/tmp}/git-ai-daemon-XXXXXX")
mkdir -p "$DAEMON_HOME/.git-ai"

# ── Write daemon config ──────────────────────────────────────────────────────
cat > "$DAEMON_HOME/.git-ai/config.json" <<CONF
{
    "git_path": "$REAL_GIT",
    "disable_auto_updates": true,
    "feature_flags": {
        "git_hooks_enabled": false
    },
    "quiet": false
}
CONF


# ── Socket paths ─────────────────────────────────────────────────────────────
CTRL_SOCK="$DAEMON_HOME/control.sock"
TRACE_SOCK="$DAEMON_HOME/trace.sock"

# ── Export env vars ──────────────────────────────────────────────────────────
export GIT_AI_TEST_FORCE_TTY=1
export GIT_AI_POST_COMMIT_TIMEOUT_MS=30000
export GIT_AI_DAEMON_HOME="$DAEMON_HOME"
export GIT_AI_DAEMON_CONTROL_SOCKET="$CTRL_SOCK"
export GIT_AI_DAEMON_TRACE_SOCKET="$TRACE_SOCK"

# Persist to GITHUB_ENV so subsequent workflow steps inherit them.
if [ -n "${GITHUB_ENV:-}" ]; then
    {
        echo "GIT_AI_TEST_FORCE_TTY=1"
        echo "GIT_AI_POST_COMMIT_TIMEOUT_MS=30000"
        echo "GIT_AI_DAEMON_HOME=$DAEMON_HOME"
        echo "GIT_AI_DAEMON_CONTROL_SOCKET=$CTRL_SOCK"
        echo "GIT_AI_DAEMON_TRACE_SOCKET=$TRACE_SOCK"
    } >> "$GITHUB_ENV"
fi

# ── Start the daemon ─────────────────────────────────────────────────────────
"$GIT_AI_BIN" bg run &
ASYNC_DAEMON_PID=$!
export ASYNC_DAEMON_PID

if [ -n "${GITHUB_ENV:-}" ]; then
    echo "ASYNC_DAEMON_PID=$ASYNC_DAEMON_PID" >> "$GITHUB_ENV"
fi

# ── Wait for sockets (up to 10 s) ───────────────────────────────────────────
for _i in $(seq 1 400); do
    [ -S "$CTRL_SOCK" ] && [ -S "$TRACE_SOCK" ] && break
    sleep 0.025
done

if [ ! -S "$CTRL_SOCK" ] || [ ! -S "$TRACE_SOCK" ]; then
    echo "ERROR: daemon sockets did not appear after 10 s" >&2
    echo "  CTRL_SOCK=$CTRL_SOCK" >&2
    echo "  TRACE_SOCK=$TRACE_SOCK" >&2
    kill -9 "$ASYNC_DAEMON_PID" 2>/dev/null || true
    exit 1
fi

echo "Async daemon started (PID=$ASYNC_DAEMON_PID)"
echo "  DAEMON_HOME=$DAEMON_HOME"
echo "  CTRL_SOCK=$CTRL_SOCK"
echo "  TRACE_SOCK=$TRACE_SOCK"
